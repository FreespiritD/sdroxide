//! The IIOD wire protocol, on one TCP connection.
//!
//! `iiod` — the daemon a Pluto runs on port 30431 — speaks a line-oriented text
//! protocol. Commands are `\r\n`-terminated ASCII; **every** reply begins with a
//! decimal integer on its own line, which is a byte count when it is positive
//! and `-errno` when it is negative. Commands that return data follow that line
//! with exactly that many bytes.
//!
//! ```text
//! VERSION                                    -> "<maj>.<min> <git-tag>"
//! PRINT                                      -> len, len bytes of XML, "\n"
//! TIMEOUT <ms>                               -> code
//! READ  <dev> [INPUT|OUTPUT <chan>] <attr>   -> len, len bytes, "\n"
//! WRITE <dev> [INPUT|OUTPUT <chan>] <attr> <len> + bytes  -> code
//! OPEN  <dev> <samples> <mask> [CYCLIC]      -> code
//! CLOSE <dev>                                -> code
//! READBUF  <dev> <bytes>   -> { len, mask, len bytes } … until len <= 0
//! WRITEBUF <dev> <bytes>   -> code, send bytes, -> code
//! EXIT
//! ```
//!
//! # The two framing details that matter
//!
//! **`READBUF` is chunked.** A request larger than the server's own buffer comes
//! back as several `{length, mask, data}` groups, and the sequence ends on a
//! length of zero or an error. Each positive-length group is preceded by the
//! channel mask, printed as `words × 8` hex characters — the same shape `OPEN`
//! takes it in. [`Connection::read_buf`] therefore reads the mask on every
//! group, not just the first.
//!
//! **`WRITE`'s length counts the bytes that follow**, and libiio appends a
//! trailing NUL to attribute strings. Writing `"40"` is three bytes, not two;
//! a length that disagrees with what follows desynchronises the connection for
//! the rest of the session, which surfaces much later as a nonsense reply to an
//! unrelated command.
//!
//! # Provenance
//!
//! The command set and framing are transcribed from libiio's `iiod-client.c`
//! (the client half) and `iiod/ops.c` (the server half), LGPL-2.1-or-later.
//! **Not verified against hardware** — see [`crate::trace`] for how a tester
//! reports what the wire actually looked like.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::trace::Trace;

/// The port `iiod` listens on.
pub const DEFAULT_PORT: u16 = 30431;

/// How long a single socket operation may block before the connection counts as
/// stalled. Generous: a `READBUF` legitimately waits for the device to fill a
/// buffer, which at the lowest offered rate (521 ksps, 32768 samples) is 63 ms,
/// and `iiod` on a busy Zynq is not always prompt.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Server-side timeout handed to `TIMEOUT`, in ms. Shorter than [`IO_TIMEOUT`]
/// so a device that stops producing is reported by the server as an error we
/// can act on, rather than by our own socket timeout, which tells us nothing
/// about whether the *device* or the *link* is at fault.
const SERVER_TIMEOUT_MS: u32 = 3_000;

/// One TCP connection to `iiod`.
///
/// IIOD is strictly request/response per connection, so a blocking `READBUF`
/// blocks everything else on the same socket. That is why [`crate::net`] opens
/// three of these — control, RX and TX — rather than multiplexing one.
pub struct Connection {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    trace: Trace,
    /// Whether the head of a buffer response has been traced yet. Only the
    /// first one is worth keeping; after that it is 8 MB/s of noise.
    traced_head: bool,
}

impl Connection {
    /// Connect and set both socket timeouts. `connect_timeout` bounds only the
    /// TCP handshake — a Pluto that is powered but still booting accepts the
    /// connection and then says nothing, which is what [`IO_TIMEOUT`] catches.
    pub fn connect(
        addr: SocketAddr,
        connect_timeout: Duration,
        trace: Trace,
    ) -> Result<Connection> {
        trace.note(format!("connecting to {addr}"));
        let sock = TcpStream::connect_timeout(&addr, connect_timeout)
            .map_err(|e| Error::from_connect(e, addr))?;
        sock.set_read_timeout(Some(IO_TIMEOUT)).map_err(|e| Error::io("set_read_timeout", e))?;
        sock.set_write_timeout(Some(IO_TIMEOUT)).map_err(|e| Error::io("set_write_timeout", e))?;
        // Attribute writes are tiny and latency-sensitive (a retune is one), and
        // Nagle would hold them back waiting for company that never comes.
        let _ = sock.set_nodelay(true);
        let writer = sock.try_clone().map_err(|e| Error::io("clone socket", e))?;
        let mut conn = Connection {
            reader: BufReader::with_capacity(64 * 1024, sock),
            writer,
            trace,
            traced_head: false,
        };
        // Tell the server how long it may wait on the device. Old firmwares
        // that don't know the command answer -EINVAL, which is not fatal.
        if let Err(e) = conn.timeout(SERVER_TIMEOUT_MS) {
            conn.trace.note(format!("TIMEOUT not accepted ({e}); continuing with the default"));
        }
        Ok(conn)
    }

    // ---- commands -------------------------------------------------------

    /// `VERSION` — the daemon's libiio version and git tag.
    pub fn version(&mut self) -> Result<String> {
        let cmd = "VERSION";
        self.send(cmd)?;
        let line = self.read_line(cmd)?;
        self.trace.reply(&line);
        Ok(line)
    }

    /// `PRINT` — the context description, as XML.
    pub fn print_xml(&mut self) -> Result<String> {
        let cmd = "PRINT";
        self.send(cmd)?;
        let n = self.read_code(cmd)?;
        let bytes = self.read_exact_n(cmd, n as usize)?;
        // The payload is followed by a newline the length does not count.
        self.consume_trailing_newline();
        self.trace.reply(format!("PRINT: {n} bytes of XML"));
        String::from_utf8(bytes)
            .map_err(|e| Error::Xml(format!("context description is not UTF-8: {e}")))
    }

    /// `TIMEOUT <ms>` — how long the server may block on the device.
    pub fn timeout(&mut self, ms: u32) -> Result<()> {
        let cmd = format!("TIMEOUT {ms}");
        self.send(&cmd)?;
        self.read_code(&cmd)?;
        Ok(())
    }

    /// `READ` a device- or channel-level attribute, as a trimmed string.
    ///
    /// `chan` is `None` for a device attribute, or `Some((output, id))` for a
    /// channel one — `output` picking the `INPUT`/`OUTPUT` keyword, which in IIO
    /// terms is the *direction of the signal*: the AD9361's receive chain is
    /// `INPUT voltage0` and its transmit chain `OUTPUT voltage0`, two different
    /// channels that happen to share a name.
    pub fn read_attr(
        &mut self,
        dev: &str,
        chan: Option<(bool, &str)>,
        attr: &str,
    ) -> Result<String> {
        let cmd = match chan {
            None => format!("READ {dev} {attr}"),
            Some((output, id)) => {
                format!("READ {dev} {} {id} {attr}", if output { "OUTPUT" } else { "INPUT" })
            }
        };
        self.send(&cmd)?;
        let n = self.read_code(&cmd)?;
        let bytes = self.read_exact_n(&cmd, n as usize)?;
        self.consume_trailing_newline();
        // Attribute values arrive NUL-terminated; the NUL is inside the count.
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_end_matches('\0').trim().to_string();
        self.trace.reply(format!("{n}: {text}"));
        Ok(text)
    }

    /// `WRITE` a device- or channel-level attribute.
    ///
    /// The value goes on the wire NUL-terminated, exactly as libiio writes it —
    /// the kernel's attribute stores accept either, but the length must match
    /// what actually follows or the connection desynchronises.
    pub fn write_attr(
        &mut self,
        dev: &str,
        chan: Option<(bool, &str)>,
        attr: &str,
        value: &str,
    ) -> Result<()> {
        let mut payload = value.as_bytes().to_vec();
        payload.push(0);
        let cmd = match chan {
            None => format!("WRITE {dev} {attr} {}", payload.len()),
            Some((output, id)) => format!(
                "WRITE {dev} {} {id} {attr} {}",
                if output { "OUTPUT" } else { "INPUT" },
                payload.len()
            ),
        };
        self.trace.cmd(&format!("{cmd}  [{value}]"));
        self.write_all_raw(&format!("{cmd}\r\n"))?;
        self.writer.write_all(&payload).map_err(|e| Error::io("write attribute value", e))?;
        self.writer.flush().map_err(|e| Error::io("flush", e))?;
        self.read_code(&cmd)?;
        Ok(())
    }

    /// `OPEN <dev> <samples> <mask> [CYCLIC]` — create a buffer of
    /// `samples` sample-sets with `channels` scan elements enabled.
    pub fn open_buffer(&mut self, dev: &str, samples: usize, channels: usize) -> Result<()> {
        let cmd = format!("OPEN {dev} {samples} {}", channel_mask(channels));
        self.send(&cmd)?;
        self.read_code(&cmd)?;
        Ok(())
    }

    /// `CLOSE <dev>`. A buffer that is not open answers `-EBADF`, which is not
    /// worth surfacing — closing twice is how the shutdown paths stay simple.
    pub fn close_buffer(&mut self, dev: &str) -> Result<()> {
        let cmd = format!("CLOSE {dev}");
        self.send(&cmd)?;
        match self.read_code(&cmd) {
            Ok(_) => Ok(()),
            // -EBADF: it was not open. Closing twice is how the shutdown paths
            // stay simple, so that is not news.
            Err(Error::Remote { code: -9, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `READBUF <dev> <bytes>` — read up to `out.len()` bytes of raw sample
    /// data, following the chunked framing described in this module's header.
    /// Returns how many bytes landed in `out`.
    ///
    /// The mask that precedes each chunk is read and checked against
    /// `channels`. A mismatch means the server is sending a different set of
    /// scan elements than we asked for, which would be decoded as garbage
    /// samples — worth failing on rather than passing off as signal.
    pub fn read_buf(&mut self, dev: &str, channels: usize, out: &mut [u8]) -> Result<usize> {
        let cmd = format!("READBUF {dev} {}", out.len());
        self.send(&cmd)?;
        let want_mask = channel_mask(channels);
        let words = mask_words(channels);
        let mut got = 0usize;
        loop {
            let n = self.read_code(&cmd)?;
            if n == 0 {
                break;
            }
            let n = n as usize;
            if got + n > out.len() {
                return Err(Error::Protocol {
                    cmd,
                    what: format!(
                        "server offered {n} more bytes than the {} requested (had {got})",
                        out.len()
                    ),
                });
            }
            let mask = self.read_mask(&cmd, words)?;
            if !self.traced_head {
                // The one line a tester sends back when the stream decodes to
                // noise: the framing, in the order it actually arrived.
                self.trace
                    .wire_head(&format!("first {cmd} chunk"), format!("{n}\n{mask}\n").as_bytes());
            }
            self.read_exact_into(&cmd, &mut out[got..got + n])?;
            if !self.traced_head {
                self.trace.wire_head("first sample bytes", &out[got..(got + n).min(got + 32)]);
                self.traced_head = true;
            }
            if mask != want_mask {
                return Err(Error::Protocol {
                    cmd,
                    what: format!(
                        "channel mask {mask} in the response does not match the {want_mask} \
                         the buffer was opened with — the sample layout is not what we would \
                         decode"
                    ),
                });
            }
            got += n;
            if got >= out.len() {
                break;
            }
        }
        Ok(got)
    }

    /// `WRITEBUF <dev> <bytes>` — hand raw sample data to the device.
    ///
    /// The server answers once before the payload (it is ready) and once after
    /// (how much it took), so this call blocks until the device has consumed
    /// the data. That is the transmit path's pacing: no separate clock is
    /// needed on our side.
    pub fn write_buf(&mut self, dev: &str, data: &[u8]) -> Result<usize> {
        let cmd = format!("WRITEBUF {dev} {}", data.len());
        self.send(&cmd)?;
        self.read_code(&cmd)?;
        self.writer.write_all(data).map_err(|e| Error::io("write buffer", e))?;
        self.writer.flush().map_err(|e| Error::io("flush", e))?;
        let n = self.read_code(&cmd)?;
        Ok(n as usize)
    }

    /// `EXIT` — ask the server to close this connection. Failures are ignored:
    /// this only ever runs on the way out.
    pub fn exit(&mut self) {
        let _ = self.write_all_raw("EXIT\r\n");
    }

    // ---- framing --------------------------------------------------------

    fn send(&mut self, cmd: &str) -> Result<()> {
        self.trace.cmd(cmd);
        self.write_all_raw(&format!("{cmd}\r\n"))
    }

    fn write_all_raw(&mut self, s: &str) -> Result<()> {
        self.writer.write_all(s.as_bytes()).map_err(|e| Error::io("write command", e))?;
        self.writer.flush().map_err(|e| Error::io("flush", e))
    }

    /// Read one reply line, skipping blank ones.
    ///
    /// The blank-line tolerance pairs with [`Self::consume_trailing_newline`],
    /// which only swallows a payload's terminating newline if it has already
    /// arrived. Between them, a newline that lands in the next TCP segment is
    /// absorbed on whichever side sees it, and neither can stall waiting for a
    /// byte a firmware might not send at all.
    fn read_line(&mut self, cmd: &str) -> Result<String> {
        for _ in 0..4 {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).map_err(|e| Error::io("read reply", e))?;
            if n == 0 {
                return Err(Error::Protocol {
                    cmd: cmd.to_string(),
                    what: "the server closed the connection".into(),
                });
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if !line.is_empty() {
                return Ok(line.to_string());
            }
        }
        Err(Error::Protocol {
            cmd: cmd.to_string(),
            what: "the server sent only blank lines".into(),
        })
    }

    /// Read the integer reply line, turning a negative value into a
    /// [`Error::Remote`] naming the command that earned it.
    fn read_code(&mut self, cmd: &str) -> Result<i64> {
        let line = self.read_line(cmd)?;
        let code: i64 = line.trim().parse().map_err(|_| Error::Protocol {
            cmd: cmd.to_string(),
            what: format!("expected a status number, got {line:?}"),
        })?;
        if code < 0 {
            return Err(Error::Remote { cmd: cmd.to_string(), code });
        }
        Ok(code)
    }

    /// Read the `words × 8` hex characters of a channel mask, plus its newline.
    fn read_mask(&mut self, cmd: &str, words: usize) -> Result<String> {
        let mut buf = vec![0u8; words * 8];
        self.read_exact_into(cmd, &mut buf)?;
        let mask = String::from_utf8_lossy(&buf).to_string();
        if !mask.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Protocol {
                cmd: cmd.to_string(),
                what: format!(
                    "expected a {}-character hex channel mask, got {mask:?} — the chunk \
                     framing is not what this client was built for",
                    words * 8
                ),
            });
        }
        self.consume_trailing_newline();
        Ok(mask.to_lowercase())
    }

    fn read_exact_n(&mut self, cmd: &str, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.read_exact_into(cmd, &mut buf)?;
        Ok(buf)
    }

    fn read_exact_into(&mut self, cmd: &str, buf: &mut [u8]) -> Result<()> {
        self.reader.read_exact(buf).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => Error::Protocol {
                cmd: cmd.to_string(),
                what: format!(
                    "the server closed the connection with {} bytes still due",
                    buf.len()
                ),
            },
            _ => Error::io("read payload", e),
        })
    }

    /// Swallow the `\n` (or `\r\n`) that follows a length-counted payload.
    ///
    /// Looks only at bytes that have *already* arrived — `buffer()` rather than
    /// `fill_buf()`. Blocking here for a newline that a firmware might not send
    /// would put the socket timeout on the end of every single command; if it
    /// instead arrives in a later segment, [`Self::read_line`] skips it.
    fn consume_trailing_newline(&mut self) {
        while matches!(self.reader.buffer().first(), Some(b'\r') | Some(b'\n')) {
            self.reader.consume(1);
        }
    }
}

/// How many 32-bit words a mask for `channels` scan elements occupies.
pub fn mask_words(channels: usize) -> usize {
    channels.div_ceil(32).max(1)
}

/// Format the enabled-channel mask for `OPEN`: one bit per scan-element index,
/// printed as 32-bit words in hex, **most significant word first**.
pub fn channel_mask(channels: usize) -> String {
    let words = mask_words(channels);
    let mut out = String::with_capacity(words * 8);
    for w in (0..words).rev() {
        let mut word: u32 = 0;
        for bit in 0..32 {
            let idx = w * 32 + bit;
            if idx < channels {
                word |= 1 << bit;
            }
        }
        out.push_str(&format!("{word:08x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn masks_are_little_endian_words_printed_big_end_first() {
        // The Pluto's two I/Q scan elements.
        assert_eq!(channel_mask(2), "00000003");
        assert_eq!(channel_mask(1), "00000001");
        assert_eq!(channel_mask(32), "ffffffff");
        // 33 channels needs a second word, and the high word prints first.
        assert_eq!(channel_mask(33), "00000001ffffffff");
        assert_eq!(mask_words(1), 1);
        assert_eq!(mask_words(32), 1);
        assert_eq!(mask_words(33), 2);
    }

    /// Stand up a one-shot server that replays `script` and records everything
    /// the client sent, so the framing can be asserted from both sides.
    fn with_server<F, T>(script: Vec<u8>, f: F) -> (T, Vec<u8>)
    where
        F: FnOnce(&mut Connection) -> T,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (sent_tx, sent_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            sock.write_all(&script).expect("write script");
            sock.flush().ok();
            // Read whatever the client sends until it hangs up, so the test can
            // check the exact bytes that went out.
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
            }
            let _ = sent_tx.send(got);
        });
        let mut conn =
            Connection::connect(addr, Duration::from_secs(2), Trace::new()).expect("connect");
        let out = f(&mut conn);
        drop(conn);
        let sent = sent_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        (out, sent)
    }

    /// The reply to the `TIMEOUT` the constructor sends, ahead of every script.
    fn preamble() -> Vec<u8> {
        b"0\r\n".to_vec()
    }

    #[test]
    fn an_attribute_read_strips_the_nul_and_the_newline() {
        let mut script = preamble();
        script.extend_from_slice(b"5\r\n40.0\0\n");
        let (value, sent) = with_server(script, |c| {
            c.read_attr("ad9361-phy", Some((false, "voltage0")), "hardwaregain")
        });
        assert_eq!(value.expect("read"), "40.0");
        let sent = String::from_utf8_lossy(&sent);
        assert!(sent.contains("READ ad9361-phy INPUT voltage0 hardwaregain\r\n"), "sent: {sent:?}");
    }

    /// The length on a `WRITE` counts the trailing NUL. Getting this wrong
    /// leaves the connection one byte out of step for the rest of the session.
    #[test]
    fn an_attribute_write_counts_the_trailing_nul() {
        let mut script = preamble();
        script.extend_from_slice(b"11\r\n");
        let (ok, sent) = with_server(script, |c| {
            c.write_attr("ad9361-phy", Some((true, "altvoltage0")), "frequency", "1000000000")
        });
        ok.expect("write");
        let sent = String::from_utf8_lossy(&sent);
        // "1000000000" is 10 bytes, plus the NUL = 11.
        assert!(
            sent.contains("WRITE ad9361-phy OUTPUT altvoltage0 frequency 11\r\n1000000000\0"),
            "sent: {sent:?}"
        );
    }

    #[test]
    fn a_negative_reply_names_the_command_and_the_errno() {
        let mut script = preamble();
        script.extend_from_slice(b"-16\r\n");
        let (res, _) = with_server(script, |c| c.open_buffer("cf-ad9361-lpc", 4096, 2));
        let err = res.expect_err("should propagate -EBUSY");
        let text = err.to_string();
        assert!(text.contains("OPEN cf-ad9361-lpc"), "{text}");
        assert!(text.contains("device busy"), "{text}");
    }

    /// The framing this client is most likely to have wrong: a `READBUF`
    /// answered as several `{length, mask, data}` groups terminated by a zero.
    #[test]
    fn readbuf_reassembles_chunks_each_preceded_by_its_mask() {
        let mut script = preamble();
        script.extend_from_slice(b"4\n00000003\n");
        script.extend_from_slice(&[1, 2, 3, 4]);
        script.extend_from_slice(b"2\n00000003\n");
        script.extend_from_slice(&[5, 6]);
        script.extend_from_slice(b"0\n");
        let mut out = [0u8; 8];
        let ((n, bytes), sent) = with_server(script, |c| {
            let n = c.read_buf("cf-ad9361-lpc", 2, &mut out).expect("read");
            (n, out)
        });
        assert_eq!(n, 6);
        assert_eq!(&bytes[..6], &[1, 2, 3, 4, 5, 6]);
        assert!(String::from_utf8_lossy(&sent).contains("READBUF cf-ad9361-lpc 8\r\n"));
    }

    /// A chunk sequence that fills the request exactly must not wait for a
    /// terminating zero the server will never send.
    #[test]
    fn readbuf_stops_once_the_request_is_satisfied() {
        let mut script = preamble();
        script.extend_from_slice(b"4\n00000003\n");
        script.extend_from_slice(&[9, 9, 9, 9]);
        let mut out = [0u8; 4];
        let (n, _) = with_server(script, |c| c.read_buf("cf-ad9361-lpc", 2, &mut out));
        assert_eq!(n.expect("read"), 4);
    }

    /// A mask that disagrees with what the buffer was opened with means the
    /// samples are not laid out the way we would decode them. Silence there
    /// produces a plausible-looking spectrum made of nonsense.
    #[test]
    fn a_mismatched_mask_is_an_error_not_a_shrug() {
        let mut script = preamble();
        script.extend_from_slice(b"4\n00000001\n");
        script.extend_from_slice(&[1, 2, 3, 4]);
        let mut out = [0u8; 4];
        let (res, _) = with_server(script, |c| c.read_buf("cf-ad9361-lpc", 2, &mut out));
        let text = res.expect_err("mask mismatch").to_string();
        assert!(text.contains("00000001"), "{text}");
        assert!(text.contains("00000003"), "{text}");
    }

    /// Text where a number belongs is the symptom of a desynchronised
    /// connection, and the message has to say so rather than "invalid digit".
    #[test]
    fn a_non_numeric_status_line_reports_what_arrived() {
        let mut script = preamble();
        script.extend_from_slice(b"HELP\r\n");
        let (res, _) = with_server(script, |c| c.timeout(1));
        let text = res.expect_err("desync").to_string();
        assert!(text.contains("expected a status number"), "{text}");
        assert!(text.contains("HELP"), "{text}");
    }

    #[test]
    fn print_returns_the_counted_xml_payload() {
        let xml = "<ctx></ctx>";
        let mut script = preamble();
        script.extend_from_slice(format!("{}\n{xml}\n", xml.len()).as_bytes());
        let (got, _) = with_server(script, |c| c.print_xml());
        assert_eq!(got.expect("print"), xml);
    }

    #[test]
    fn writebuf_sends_the_payload_between_the_two_status_lines() {
        let mut script = preamble();
        script.extend_from_slice(b"0\n"); // ready
        script.extend_from_slice(b"4\n"); // accepted 4 bytes
        let (n, sent) =
            with_server(script, |c| c.write_buf("cf-ad9361-dds-core-lpc", &[1, 2, 3, 4]));
        assert_eq!(n.expect("write"), 4);
        let sent = String::from_utf8_lossy(&sent);
        assert!(sent.contains("WRITEBUF cf-ad9361-dds-core-lpc 4\r\n"), "sent: {sent:?}");
        assert!(sent.ends_with("\u{1}\u{2}\u{3}\u{4}"), "sent: {sent:?}");
    }

    /// Closing a buffer that was never open is how the shutdown paths stay
    /// simple; -EBADF there must not become a user-visible error.
    #[test]
    fn closing_an_unopened_buffer_is_not_a_failure() {
        let mut script = preamble();
        script.extend_from_slice(b"-9\n");
        let (res, _) = with_server(script, |c| c.close_buffer("cf-ad9361-lpc"));
        assert!(res.is_ok());
    }
}
