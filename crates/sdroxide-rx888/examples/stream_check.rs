//! End-to-end check of the full backend: USB thread, converter thread, and the
//! ring the engine will read from.
//!
//! ```text
//! cargo run --release -p sdroxide-rx888 --example stream_check -- 13.635
//! ```
//!
//! Reports the achieved complex rate against the nominal one. That ratio is the
//! whole point: it is the number that says whether the two threads keep up with
//! 129.6 MB/s in real time, which no amount of offline replay can tell you.

use std::f64::consts::TAU;
use std::time::{Duration, Instant};

use sdroxide_rx888::{Settings, spawn};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sdroxide_rx888=info".into()),
        )
        .init();

    let mhz: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(13.635);
    let secs: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5.0);

    let settings = Settings { vga_db: 18.0, ..Default::default() };
    let mut h = match spawn(&settings, mhz * 1e6) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("failed to start: {e}");
            std::process::exit(1);
        }
    };
    if let Some(w) = h.warning() {
        println!("WARNING: {w}");
    }
    println!(
        "{} (serial {})\n  ADC {:.3} Msps real -> {:.4} Msps complex, tuning grid {:.2} kHz",
        h.label(),
        h.serial().unwrap_or("none"),
        h.adc_rate_hz() / 1e6,
        h.out_rate_hz() / 1e6,
        h.bin_hz() / 1e3,
    );
    println!("  tuned to {mhz:.4} MHz, reading for {secs:.1} s\n");

    let mut buf = vec![0f32; 1 << 16];
    let mut total = 0usize;
    let mut sum_sq = 0f64;
    let mut peak = 0f32;
    // Keep a slice of samples for a frequency check.
    let mut keep: Vec<(f32, f32)> = Vec::with_capacity(1 << 16);

    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        let n = h.read(&mut buf);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        total += n / 2;
        for c in buf[..n].chunks_exact(2) {
            let (re, im) = (c[0], c[1]);
            let p = (re * re + im * im) as f64;
            sum_sq += p;
            peak = peak.max(p.sqrt() as f32);
            if keep.len() < keep.capacity() {
                keep.push((re, im));
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let achieved = total as f64 / elapsed;

    println!("read {total} complex samples in {elapsed:.2} s");
    println!(
        "  achieved {:.4} Msps = {:.1}% of nominal",
        achieved / 1e6,
        100.0 * achieved / h.out_rate_hz()
    );
    println!("  dropped by the ring: {}", h.dropped());
    let rms = (sum_sq / total.max(1) as f64).sqrt();
    println!(
        "  rms {:.1} dBFS, peak {:.1} dBFS",
        20.0 * rms.max(1e-12).log10(),
        20.0 * peak.max(1e-12).log10()
    );

    // Where the energy sits inside the channel — a carrier should sit at DC if
    // the tuning is right.
    if keep.len() > 4096 {
        let fs = h.out_rate_hz();
        let mut best = (0.0f64, 0.0f32);
        for s in -240i32..=240 {
            let f = s as f64 * fs / 2.0 / 240.0 * 0.75;
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, (a, b)) in keep.iter().enumerate() {
                let ph = -TAU * f * i as f64 / fs;
                let (c, d) = (ph.cos() as f32, ph.sin() as f32);
                re += a * c - b * d;
                im += a * d + b * c;
            }
            let mag = (re * re + im * im).sqrt() / keep.len() as f32;
            if mag > best.1 {
                best = (f, mag);
            }
        }
        println!(
            "  strongest component {:+.1} kHz from centre = {:.4} MHz at {:.1} dBFS",
            best.0 / 1e3,
            (mhz * 1e6 + best.0) / 1e6,
            20.0 * best.1.max(1e-12).log10()
        );
    }

    h.release();
    println!("\nreleased cleanly, alive = {}", h.is_alive());
}
