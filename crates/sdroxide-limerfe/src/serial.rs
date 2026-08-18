//! The LimeRFE over its own micro-USB port.
//!
//! Pure Rust, and deliberately so rather than calling LimeSuite's
//! `RFE_Open(port, NULL)`: this is the transport that works on a machine with
//! no LimeSuite installed at all, which is what lets a LimeRFE sit in front of
//! something that is not a LimeSDR. It also means the timeouts are ours —
//! `RFE_Open` blocks for up to two seconds doing its own handshake, on
//! whichever thread called it, with no way to give up early.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use sdroxide_types::{RFE_BAUD, RFE_BUFFER_SIZE, RfeMode};

use crate::error::{Error, Result};
use crate::frame::{self, HELLO_INTERVAL_MS, MAX_HELLO_ATTEMPTS, RfeInfo, RfeState, cmd};
use crate::transport::RfeTransport;

/// How long to wait for the board's 16 bytes. Generous next to the ~17 ms the
/// bytes themselves take at 9600 baud, because the board answers after it has
/// finished throwing its relays.
const REPLY_TIMEOUT: Duration = Duration::from_millis(600);

/// The serial read timeout. Short, so a partial reply is noticed and retried
/// inside [`REPLY_TIMEOUT`] rather than blocking to the end of it.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// One exchange, near enough. 16 bytes each way at 9600 8N1 is ~33 ms of wire
/// time; the relays add the rest.
const ROUND_TRIP: Duration = Duration::from_millis(45);

pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
    path: String,
    info: RfeInfo,
}

impl SerialTransport {
    /// Open the port and shake hands.
    ///
    /// The handshake is not ceremony: the board's microcontroller can still be
    /// booting when the CDC port enumerates, and an FTDI bridge will happily
    /// hand over a port with nothing behind it. Ten tries 200 ms apart is
    /// LimeSuite's own patience, and it is what tells a LimeRFE apart from the
    /// several other things that present the same generic USB-serial id.
    pub fn open(path: &str) -> Result<SerialTransport> {
        let port = serialport::new(path, RFE_BAUD)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .timeout(READ_TIMEOUT)
            .open()
            .map_err(|e| Error::Open {
                path: path.to_string(),
                source: std::io::Error::other(e),
            })?;

        let mut t = SerialTransport {
            port,
            path: path.to_string(),
            info: RfeInfo { firmware: 0, hardware: 0 },
        };

        for attempt in 0..MAX_HELLO_ATTEMPTS {
            // Anything the bridge buffered before we arrived is not an answer
            // to a question we asked.
            let _ = t.port.clear(serialport::ClearBuffer::All);
            if t.txn(frame::encode_bare(cmd::HELLO)).is_ok() {
                t.info = t.info()?;
                tracing::info!(
                    "LimeRFE on {path}: firmware {}, hardware {}",
                    t.info.firmware,
                    t.info.hardware
                );
                return Ok(t);
            }
            if attempt + 1 < MAX_HELLO_ATTEMPTS {
                std::thread::sleep(Duration::from_millis(HELLO_INTERVAL_MS));
            }
        }
        Err(Error::NoAnswer { path: path.to_string() })
    }

    /// Send one frame and read the reply that echoes it.
    ///
    /// A reply that does not echo the command is *discarded and reading
    /// continues* rather than being returned as an error: on a link that got
    /// out of step, the answer we want is usually right behind the stale one,
    /// and resyncing here beats surfacing a desync the caller can only retry.
    fn txn(&mut self, out: [u8; RFE_BUFFER_SIZE]) -> Result<[u8; RFE_BUFFER_SIZE]> {
        self.port.write_all(&out)?;
        self.port.flush()?;

        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut buf = [0u8; RFE_BUFFER_SIZE];
        let mut have = 0usize;
        loop {
            if have == RFE_BUFFER_SIZE {
                if buf[0] == out[0] {
                    frame::check_reply(out[0], &buf)?;
                    return Ok(buf);
                }
                // Stale frame from an earlier exchange. Drop it and keep
                // reading — but only while there is time left to.
                tracing::debug!(
                    "LimeRFE {}: discarding a stale {:#04x} reply while waiting for {:#04x}",
                    self.path,
                    buf[0],
                    out[0]
                );
                have = 0;
                continue;
            }
            if Instant::now() >= deadline {
                return Err(Error::ShortReply { want: RFE_BUFFER_SIZE, got: have });
            }
            match self.port.read(&mut buf[have..]) {
                Ok(0) => {}
                Ok(n) => have += n,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl RfeTransport for SerialTransport {
    fn info(&mut self) -> Result<RfeInfo> {
        let reply = self.txn(frame::encode_bare(cmd::GET_INFO))?;
        frame::decode_info(&reply)
    }

    fn configure(&mut self, state: RfeState) -> Result<()> {
        self.txn(frame::encode_config(&state)).map(|_| ())
    }

    fn set_mode(&mut self, mode: RfeMode) -> Result<()> {
        self.txn(frame::encode_mode(mode)).map(|_| ())
    }

    fn set_fan(&mut self, on: bool) -> Result<()> {
        self.txn(frame::encode_fan(on)).map(|_| ())
    }

    fn round_trip(&self) -> Duration {
        ROUND_TRIP
    }

    fn describe(&self) -> String {
        format!(
            "LimeRFE on {} (firmware {}, hardware {})",
            self.path, self.info.firmware, self.info.hardware
        )
    }
}
