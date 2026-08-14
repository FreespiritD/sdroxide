//! How a B2F session reaches the other end.
//!
//! Today there is one implementation — the CMS telnet gateway over the
//! internet. The trait exists now, before it has a second implementer, because
//! the whole point of the phased plan is that a packet or ARDOP link drops in
//! here without [`crate::session`] learning anything about radios.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The Winlink CMS telnet gateway.
pub const CMS_ADDRESS: &str = "server.winlink.org:8772";
/// The gateway's own shared password — not the operator's account password.
/// This one gets the socket open; the account password answers the `;PQ:`
/// challenge later, inside the B2F handshake.
pub const CMS_PASSWORD: &str = "CMSTelnet";
/// Who we are forwarding with, once connected.
pub const CMS_TARGET_CALL: &str = "wl2k";

/// A bidirectional byte stream to a forwarding partner, plus the callsign that
/// partner answers to.
pub trait Transport: Read + Write + Send {
    /// The station we are forwarding with — `wl2k` for the CMS.
    fn target_call(&self) -> &str;
    /// Human-readable, for the session log.
    fn describe(&self) -> String;
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("connecting to {0}: {1}")]
    Connect(String, std::io::Error),
    #[error("telnet login: {0}")]
    Io(#[from] std::io::Error),
    #[error("telnet login: connection closed before a password prompt arrived")]
    NoPrompt,
}

/// A CMS connection over plain TCP.
pub struct TelnetTransport {
    stream: TcpStream,
    address: String,
    /// The gateway login exchange, for the front of the session transcript.
    /// The gateway password is a published constant, not a secret, but the
    /// operator's callsign line is echoed here and nothing else is.
    login_log: Vec<String>,
}

impl TelnetTransport {
    /// What the gateway said while logging in.
    pub fn login_log(&self) -> &[String] {
        &self.login_log
    }

    /// Dial the CMS and get through its callsign/password prompts, leaving the
    /// stream positioned at the start of the B2F handshake.
    ///
    /// `callsign` is the operator's; the password here is the gateway's fixed
    /// [`CMS_PASSWORD`], which is why this function does not take one.
    pub fn dial(address: &str, callsign: &str, timeout: Duration) -> Result<Self, TransportError> {
        let addr = address
            .to_socket_addrs()
            .map_err(|e| TransportError::Connect(address.into(), e))?
            .next()
            .ok_or_else(|| {
                TransportError::Connect(
                    address.into(),
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no address"),
                )
            })?;

        let stream = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|e| TransportError::Connect(address.into(), e))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;

        let login_log = login(&stream, callsign)?;

        Ok(TelnetTransport { stream, address: address.to_string(), login_log })
    }
}

/// Answer the gateway's login prompts, returning the exchange for the log.
///
/// The prompts are `\r`-terminated and matched case-insensitively on their
/// first word: the exact wording has changed over the years, the leading token
/// has not.
///
/// Reads **one byte at a time** rather than through a `BufReader`.
///
/// A buffered reader would be free to pull in whatever else has arrived, and
/// the bytes after the password prompt are the start of the B2F handshake —
/// which belongs to the session, not to us. Anything this function swallowed
/// would be silently lost when it returns, and the symptom is a session that
/// sees no SID for reasons nothing in the transcript explains.
fn login(stream: &TcpStream, callsign: &str) -> Result<Vec<String>, TransportError> {
    let mut reader = stream.try_clone()?;
    let mut writer = stream.try_clone()?;
    let mut log = Vec::new();
    let mut line = Vec::new();

    loop {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte)? == 0 {
            return Err(TransportError::NoPrompt);
        }
        if byte[0] != b'\r' && byte[0] != b'\n' {
            line.push(byte[0]);
            continue;
        }

        let text = String::from_utf8_lossy(&line).trim().to_string();
        line.clear();
        if text.is_empty() {
            continue;
        }
        log.push(format!("< {text}"));

        let lower = text.to_lowercase();
        if lower.starts_with("callsign") {
            write!(writer, "{callsign}\r")?;
            writer.flush()?;
            log.push(format!("> {callsign}"));
        } else if lower.starts_with("password") {
            write!(writer, "{CMS_PASSWORD}\r")?;
            writer.flush()?;
            log.push(format!("> {CMS_PASSWORD}"));
            return Ok(log);
        }
    }
}

impl Read for TelnetTransport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for TelnetTransport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

impl Transport for TelnetTransport {
    fn target_call(&self) -> &str {
        CMS_TARGET_CALL
    }
    fn describe(&self) -> String {
        format!("CMS telnet {}", self.address)
    }
}
