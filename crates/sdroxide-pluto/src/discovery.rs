//! Finding a Pluto, and proving one is there.
//!
//! Two mechanisms, because neither alone is enough:
//!
//! - **mDNS.** `iiod` publishes `_iio._tcp` over Avahi, which finds a Pluto on
//!   a real LAN and finds several at once.
//! - **The default address.** The USB cable presents a private two-host network
//!   with the device on `192.168.2.1`, and multicast across it is exactly the
//!   sort of traffic a host firewall drops without comment. So that address is
//!   always tried directly, whatever mDNS said.
//!
//! Every candidate is then *opened* — no device is reported on the strength of
//! a service announcement alone, because "there is an `iiod` there" and "there
//! is a Pluto there" are different claims and the settings UI makes the second
//! one.

use std::collections::BTreeMap;
use std::time::Duration;

use sdroxide_types::PlutoDevice;

use crate::context::Context;
use crate::error::Result;
use crate::iiod::Connection;
use crate::mdns;
use crate::net::CONNECT_TIMEOUT;
use crate::phy::{PHY, Phy};
use crate::trace::Trace;

/// How many mDNS answers are worth opening. A hall full of IIO devices is not
/// the case to optimise for, and each probe costs a connection.
const MAX_CANDIDATES: usize = 8;

/// Open `address` far enough to say what is there. Cheap: a version and the
/// context description, no front-end reads.
pub fn probe(address: &str, timeout: Duration) -> Result<PlutoDevice> {
    let addr = crate::net::resolve(address)?;
    let trace = Trace::new();
    let mut conn = Connection::connect(addr, timeout, trace)?;
    let version = conn.version()?;
    let ctx = Context::parse(&conn.print_xml()?)?;
    conn.exit();
    // An IIO device that is not a Pluto is a real thing to find on a LAN — a
    // wattmeter, a lab instrument — and reporting it as a radio would be worse
    // than not reporting it at all.
    ctx.require(PHY, &addr.to_string())?;
    Ok(PlutoDevice {
        ip: addr.ip().to_string(),
        hostname: String::new(),
        model: ctx.hw_model().to_string(),
        firmware: ctx.fw_version().to_string(),
        serial: ctx.hw_serial().to_string(),
        iiod_version: version,
    })
}

/// Scan for Plutos: everything mDNS answers with, plus the USB gadget's default
/// address, each opened to confirm what it is.
pub fn discover(timeout: Duration) -> Vec<PlutoDevice> {
    // Two thirds of the budget listening, the rest opening what answered.
    let listen = timeout.mul_f32(0.65);
    let found = mdns::query(listen);
    tracing::debug!("PlutoSDR: mDNS returned {} answer(s)", found.len());

    let mut candidates: Vec<(String, String)> = Vec::new();
    for f in found.into_iter().take(MAX_CANDIDATES) {
        let port = f.port.unwrap_or(crate::DEFAULT_PORT);
        candidates.push((format!("{}:{port}", f.ip), f.name));
    }
    // Always, and last so an mDNS answer for the same address wins the name.
    candidates.push((crate::DEFAULT_ADDRESS.to_string(), "pluto".to_string()));

    let mut out: BTreeMap<String, PlutoDevice> = BTreeMap::new();
    for (address, hostname) in candidates {
        match probe(&address, CONNECT_TIMEOUT.min(timeout)) {
            Ok(mut dev) => {
                dev.hostname = hostname;
                tracing::info!("PlutoSDR: found {} at {}", dev.model, dev.ip);
                out.entry(dev.ip.clone()).or_insert(dev);
            }
            Err(e) => tracing::debug!("PlutoSDR: {address} is not a Pluto: {e}"),
        }
    }
    out.into_values().collect()
}

/// Open `address` and report what is on the other end, or why not — the
/// "Test connection" button on the Radio tab.
///
/// Deliberately heavier than [`probe`]: it reads the front-end limits too, so
/// the answer states the tuning range this particular board has rather than the
/// one its family might.
pub fn test_connection(address: &str, timeout: Duration) -> std::result::Result<String, String> {
    let trace = Trace::new();
    crate::trace::remember_probe(&trace);
    test_inner(address, timeout, &trace).map_err(|e| {
        // Keep the failure in the remembered trace: the session report has to
        // say why it stopped, not just where.
        trace.note(format!("!! test failed: {e}"));
        e.to_string()
    })
}

fn test_inner(address: &str, timeout: Duration, trace: &Trace) -> Result<String> {
    let addr = crate::net::resolve(address)?;
    let mut conn = Connection::connect(addr, timeout, trace.clone())?;
    let version = conn.version()?;
    let ctx = Context::parse(&conn.print_xml()?)?;
    trace.set_context(ctx.summary());
    let phy = Phy::probe(&mut conn, &ctx, &addr.to_string())?;
    conn.exit();

    let model = if ctx.hw_model().is_empty() { "IIO device" } else { ctx.hw_model() };
    let mut s = format!(
        "{model}, firmware {} (iiod {version}) — {:.3}–{:.3} MHz, up to {:.3} Msps",
        if ctx.fw_version().is_empty() { "unknown" } else { ctx.fw_version() },
        phy.limits.rx_lo_hz.0 / 1e6,
        phy.limits.rx_lo_hz.1 / 1e6,
        phy.limits.sample_rate_hz.1 / 1e6,
    );
    if !phy.limits.assumed.is_empty() {
        // Say which of those numbers the board did not actually state.
        s.push_str(&format!(" (assumed: {})", phy.limits.assumed.join(", ")));
    }
    Ok(s)
}
