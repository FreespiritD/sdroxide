//! Connect to the live Winlink CMS and run one forwarding session.
//!
//! A development harness, in the spirit of the other crates' `probe` examples:
//! it exercises the whole client stack — telnet login, B2F handshake, secure
//! login, proposals, framing, LZHUF — against the real thing, and prints the
//! protocol transcript so a mismatch is visible rather than merely fatal.
//!
//! Credentials come from the environment so they stay out of the repository
//! and out of shell history:
//!
//! ```text
//! WINLINK_CALLSIGN=OE3JJS WINLINK_PASSWORD=... \
//!     cargo run -p sdroxide-winlink --example cms_probe
//! ```
//!
//! With `--send` it also posts a short message to the account itself, which is
//! the end-to-end check: it should come back on a later connection.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sdroxide_winlink::message::{Message, format_date, generate_mid};
use sdroxide_winlink::session::{self, SessionConfig};
use sdroxide_winlink::transport::{CMS_ADDRESS, TelnetTransport};

fn main() {
    let callsign =
        std::env::var("WINLINK_CALLSIGN").expect("set WINLINK_CALLSIGN").trim().to_uppercase();
    let password = std::env::var("WINLINK_PASSWORD").expect("set WINLINK_PASSWORD");
    let locator = std::env::var("WINLINK_LOCATOR").unwrap_or_else(|_| "JN88".into());
    let send = std::env::args().any(|a| a == "--send");
    // Production refuses client names it does not know, and says so, naming
    // cms-z.winlink.org as the host for everyone else. Override to develop
    // against that until "sdroxide" is a registered client type.
    let address = std::env::var("WINLINK_CMS").unwrap_or_else(|_| CMS_ADDRESS.into());
    // Some implementations announce themselves as a known client to get in.
    // Kept as an explicit, off-by-default knob rather than a silent default:
    // claiming to be somebody else's client is not ours to do quietly.
    let app_name = std::env::var("WINLINK_APP_NAME").unwrap_or_else(|_| "sdroxide".into());

    println!("connecting to {address} as {callsign} (client name {app_name})…");
    let mut transport = match TelnetTransport::dial(&address, &callsign, Duration::from_secs(30)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("dial failed: {e}");
            std::process::exit(1);
        }
    };
    println!("logged in to the gateway, starting B2F…\n");

    let cfg = SessionConfig {
        callsign: callsign.clone(),
        password,
        locator,
        app_name,
        app_version: env!("CARGO_PKG_VERSION").into(),
    };

    let outbound = if send { vec![test_message(&callsign)] } else { Vec::new() };
    if let Some(msg) = outbound.first() {
        println!("offering a test message to {} (mid {})\n", msg.to[0], msg.mid);
    }

    let mut outcome = session::SessionOutcome::default();
    outcome.log.extend(transport.login_log().iter().cloned());
    let result = session::run_into(&mut transport, &cfg, &outbound, &mut outcome);

    // Print the transcript first and unconditionally: on a failure it is the
    // whole diagnosis.
    println!("--- transcript ---");
    for line in &outcome.log {
        println!("{line}");
    }

    if let Err(e) = result {
        eprintln!("\nsession failed: {e}");
        std::process::exit(2);
    }

    println!("\n--- result ---");
    println!("received {} message(s), sent {}", outcome.received.len(), outcome.sent.len());
    for msg in &outcome.received {
        println!(
            "\n  from {}  to {:?}\n  date {}  mid {}\n  subject: {}\n  body ({} bytes):\n{}",
            msg.from,
            msg.to,
            msg.date,
            msg.mid,
            msg.subject,
            msg.body.len(),
            indent(&msg.body)
        );
        for att in &msg.attachments {
            println!("  attachment: {} ({} bytes)", att.name, att.data.len());
        }
    }
    if !outcome.rejected.is_empty() {
        println!("rejected by the CMS: {:?}", outcome.rejected);
    }
}

fn test_message(callsign: &str) -> Message {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    Message {
        mid: generate_mid(callsign, now.as_nanos()),
        date: format_date(now.as_secs() as i64),
        msg_type: "Private".into(),
        from: callsign.to_string(),
        to: vec![callsign.to_string()],
        cc: vec![],
        subject: "sdroxide test".into(),
        mbo: callsign.to_string(),
        body: "Sent by sdroxide's Winlink client over the CMS telnet gateway.\r\n".into(),
        attachments: vec![],
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
}
