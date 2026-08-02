//! Capture live IQ from the connected TCI server and search RTTY parameters
//! against it. Diagnostic tool for issue #23.
//!
//! It deliberately does *not* call `set_center` — the rig may be shared with a
//! running sdroxide session, and retuning someone's receiver out from under
//! them to run a test is not on. It captures wherever the rig already is and
//! takes the centre frequency from the TCI status burst.
//!
//! The search leans on the decoder's frame-lock gate: a wrong shift, baud or
//! centre produces almost no output at all now, so ranking candidates by how
//! much text they produce is enough to find the right ones.
//!
//! `cargo run --release --example tci_rtty_probe -- [seconds]`

use sdroxide_dsp::{Complex32, RttyRx, bandpass_taps};
use sdroxide_tci::{TciHandle, TciUpdate};

const IQ_RATE: f64 = 192_000.0;
const AUDIO_RATE: f64 = 8_000.0;
const DECIM: usize = (IQ_RATE / AUDIO_RATE) as usize; // 24

fn main() -> anyhow::Result<()> {
    let secs: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let address = "127.0.0.1:50001";

    eprintln!("connecting to TCI at {address} (not retuning the rig)…");
    let mut handle = TciHandle::connect(address, IQ_RATE)?;
    eprintln!("device: {}", handle.device);

    let want = (IQ_RATE * secs) as usize;
    let mut iq: Vec<Complex32> = Vec::with_capacity(want);
    let mut scratch = vec![0.0f32; 65536];
    let mut center = f64::NAN;
    let start = std::time::Instant::now();
    while iq.len() < want && start.elapsed().as_secs_f64() < secs * 2.0 + 15.0 {
        for u in handle.poll_updates() {
            if let TciUpdate::Freq(hz) = u {
                center = hz;
            }
        }
        let n = handle.rx_read(&mut scratch);
        if n == 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        for p in 0..n / 2 {
            iq.push(Complex32::new(scratch[2 * p], scratch[2 * p + 1]));
        }
        if iq.len() % (IQ_RATE as usize * 5) < 65536 {
            eprintln!("  {:.0}s captured…", iq.len() as f64 / IQ_RATE);
        }
    }
    eprintln!("captured {:.1}s of IQ, rig centre {center:.0} Hz", iq.len() as f64 / IQ_RATE);
    if iq.len() < IQ_RATE as usize * 2 {
        anyhow::bail!("not enough IQ — is the rig streaming?");
    }

    // Decimate 192k → 8k complex, then take the real part: the wanted signal is
    // a few hundred Hz above the rig centre, and its mirror lands far enough
    // away that the decoder's own front end rejects it.
    let taps = bandpass_taps(481, -2400.0, 2400.0, IQ_RATE);
    let mut audio = Vec::with_capacity(iq.len() / DECIM);
    let mut i = 0;
    while i + taps.len() <= iq.len() {
        let mut acc = Complex32::new(0.0, 0.0);
        for (k, &t) in taps.iter().enumerate() {
            acc += iq[i + k] * t;
        }
        audio.push(acc.re * 8.0);
        i += DECIM;
    }
    // Complex baseband, both sides of the rig centre. Welch-averaged: a single
    // long transform has 0.1 Hz resolution, so sampling it on a coarse grid
    // just returns noise between the bins. Averaging the power of many short
    // segments gives the band power actually present.
    let mut bb = Vec::with_capacity(iq.len() / DECIM);
    let mut j = 0;
    while j + taps.len() <= iq.len() {
        let mut acc = Complex32::new(0.0, 0.0);
        for (k, &t) in taps.iter().enumerate() {
            acc += iq[j + k] * t;
        }
        bb.push(acc);
        j += DECIM;
    }
    let rms = (bb.iter().map(|z| z.norm_sqr()).sum::<f32>() / bb.len() as f32).sqrt();
    eprintln!(
        "baseband: {} samples ({:.1}s), rms {rms:.5}",
        bb.len(),
        bb.len() as f64 / AUDIO_RATE
    );

    let binw = 25.0f64;
    let seglen = (AUDIO_RATE / binw) as usize; // 320 samples -> 25 Hz bins
    let nseg = (bb.len() / seglen).max(1);
    eprintln!("\ncomplex baseband +/-2.5 kHz, {binw:.0} Hz bins, {nseg} segments averaged:");
    let mut cbins: Vec<(f64, f64)> = Vec::new();
    let mut f = -2500.0f64;
    while f <= 2500.0 {
        let w = std::f64::consts::TAU * f / AUDIO_RATE;
        let mut pow = 0.0f64;
        for sgi in 0..nseg {
            let seg = &bb[sgi * seglen..(sgi + 1) * seglen];
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for (n, z) in seg.iter().enumerate() {
                let ph = -w * n as f64;
                let (c, sn) = (ph.cos(), ph.sin());
                sr += z.re as f64 * c - z.im as f64 * sn;
                si += z.re as f64 * sn + z.im as f64 * c;
            }
            pow += sr * sr + si * si;
        }
        cbins.push((f, pow / nseg as f64));
        f += binw;
    }
    let cpk = cbins.iter().map(|b| b.1).fold(0.0f64, f64::max).max(1e-30);
    for (f, p) in &cbins {
        let db = 10.0 * (p / cpk).log10();
        if db > -35.0 {
            eprintln!(
                "  {f:>7.0} Hz {db:>6.1} {}",
                "#".repeat(((db + 35.0) / 35.0 * 45.0) as usize)
            );
        }
    }

    // Rank parameter combinations by how much text survives the lock gate.
    eprintln!("\nsearching (centre, shift, baud, reverse)…");
    let mut results: Vec<(usize, String, String)> = Vec::new();
    for &shift in &[170.0f64, 425.0, 450.0, 500.0, 575.0, 850.0] {
        for &baud in &[45.45f64, 50.0, 75.0] {
            for &rev in &[false, true] {
                let mut c = 300.0f64;
                while c <= 2200.0 {
                    let mut rx = RttyRx::new(AUDIO_RATE, c, baud, shift);
                    rx.set_reverse(rev);
                    let mut out = String::new();
                    for ch in audio.chunks(512) {
                        out.push_str(&rx.process(ch));
                    }
                    if out.chars().count() >= 20 {
                        let tag = format!(
                            "centre {c:>6.0} Hz  shift {shift:>4.0}  baud {baud:>5.2}  rev {}",
                            if rev { "on " } else { "off" }
                        );
                        results.push((out.chars().count(), tag, out));
                    }
                    c += 50.0;
                }
            }
        }
    }
    results.sort_by(|a, b| b.0.cmp(&a.0));
    if results.is_empty() {
        eprintln!("nothing decoded under any combination.");
    }
    for (n, tag, text) in results.iter().take(12) {
        let sample: String = text.chars().take(160).collect();
        println!("{n:>5} chars  {tag}\n        {sample:?}");
    }
    Ok(())
}
