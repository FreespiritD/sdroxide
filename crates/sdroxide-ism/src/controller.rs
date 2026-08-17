//! Threading wrapper around the channel decoders, mirroring
//! `sdroxide_skimmer::SkimmerController`: the realtime engine thread ships
//! window-rate IQ blocks over a bounded channel (dropping on backpressure) and
//! drains results non-blocking via [`IsmController::poll`]. Every downconverter,
//! gate and slicer runs on the worker.
//!
//! Control traffic rides a separate unbounded channel for the same reason it does
//! in the skimmer: a retune or an off-switch must never be dropped behind a
//! backed-up IQ queue, which is exactly when it is most likely to be sent.
//!
//! # The device table
//!
//! The worker keeps one [`IsmReport`] per device and re-sends the whole table a
//! couple of times a second, rather than forwarding each decode as it happens.
//! See the note on [`sdroxide_types::IsmReport`] for why that is the shape the
//! panel wants; from this side it also bounds the message size on a busy band and
//! means a dropped result costs nothing, because the next snapshot carries the
//! same information.

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use sdroxide_dsp::{Complex32 as C32, Ddc};
use sdroxide_types::{IsmChannelStatus, IsmProtocol, IsmReport, IsmSettings, IsmStatus};
use tracing::info;

use crate::gate::{Burst, Gate};
use crate::plan;
use crate::proto::{self, Scratch};

/// How often a snapshot goes out. Fast enough that a sensor's reading appears
/// while the operator is still looking at the burst on the waterfall, slow enough
/// that the table is not rebuilt for nothing on a quiet band.
const EMIT_INTERVAL: Duration = Duration::from_millis(500);

/// Actions drained from the decoder each engine tick.
pub enum IsmAction {
    Reports(Vec<IsmReport>),
    Status(IsmStatus),
}

/// Realtime data, dropped on backpressure.
struct Iq(Vec<C32>);

/// Control traffic, never dropped.
enum Ctl {
    /// The window moved: the front end retuned, or changed rate.
    Window {
        center_hz: f64,
        rate_hz: f64,
    },
    Config(IsmSettings),
    Stop,
}

pub struct IsmController {
    iq_tx: Sender<Iq>,
    ctl_tx: Sender<Ctl>,
    res_rx: Receiver<IsmAction>,
    worker: Option<JoinHandle<()>>,
}

impl IsmController {
    /// `window_rate_hz` is the rate of the IQ the engine will feed, and
    /// `window_center_hz` the absolute RF frequency that IQ is centred on.
    pub fn new(window_center_hz: f64, window_rate_hz: f64, cfg: IsmSettings) -> IsmController {
        let (iq_tx, iq_rx) = bounded::<Iq>(64);
        let (ctl_tx, ctl_rx) = unbounded::<Ctl>();
        let (res_tx, res_rx) = unbounded::<IsmAction>();

        let worker = std::thread::Builder::new()
            .name("sdroxide-ism".into())
            .spawn(move || {
                let mut w = Worker::new(window_center_hz, window_rate_hz, cfg);
                let mut last_emit = Instant::now();
                loop {
                    select! {
                        recv(ctl_rx) -> msg => match msg {
                            Ok(Ctl::Window { center_hz, rate_hz }) => {
                                w.set_window(center_hz, rate_hz);
                            }
                            Ok(Ctl::Config(next)) => w.set_config(next),
                            Ok(Ctl::Stop) | Err(_) => break,
                        },
                        recv(iq_rx) -> msg => match msg {
                            Ok(Iq(iq)) => {
                                w.process(&iq);
                                if last_emit.elapsed() >= EMIT_INTERVAL {
                                    last_emit = Instant::now();
                                    if res_tx.send(IsmAction::Reports(w.snapshot())).is_err() {
                                        break;
                                    }
                                    if res_tx.send(IsmAction::Status(w.status())).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        },
                    }
                }
            })
            .expect("spawn ism worker");

        IsmController { iq_tx, ctl_tx, res_rx, worker: Some(worker) }
    }

    /// Realtime path: hand a block of window-rate IQ to the worker. Non-blocking;
    /// drops the block if the worker is behind.
    pub fn on_rx_iq(&self, iq: &[C32]) {
        let _ = self.iq_tx.try_send(Iq(iq.to_vec()));
    }

    /// The window moved. Rebuilds the channel decoders and clears their gates —
    /// both the signal and the noise are about to be somebody else's.
    pub fn set_window(&self, center_hz: f64, rate_hz: f64) {
        let _ = self.ctl_tx.send(Ctl::Window { center_hz, rate_hz });
    }

    pub fn set_config(&self, cfg: IsmSettings) {
        let _ = self.ctl_tx.send(Ctl::Config(cfg));
    }

    /// Drain whatever the worker has produced since the last poll. Non-blocking.
    pub fn poll(&self) -> Vec<IsmAction> {
        let mut out = Vec::new();
        while let Ok(a) = self.res_rx.try_recv() {
            out.push(a);
        }
        out
    }
}

impl Drop for IsmController {
    fn drop(&mut self) {
        let _ = self.ctl_tx.send(Ctl::Stop);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

/// One channel being listened to.
struct Chan {
    ddc: Ddc,
    gate: Gate,
    buf: Vec<C32>,
}

struct Worker {
    window_center_hz: f64,
    window_rate_hz: f64,
    cfg: IsmSettings,
    chans: Vec<Chan>,
    /// Every channel in the plan and why it is or is not being listened to,
    /// rebuilt whenever the window or the settings change.
    status_chans: Vec<IsmChannelStatus>,
    devices: HashMap<u64, IsmReport>,
    scratch: Scratch,
    bursts_out: Vec<Burst>,
    decodes: u64,
}

impl Worker {
    fn new(window_center_hz: f64, window_rate_hz: f64, cfg: IsmSettings) -> Worker {
        let mut w = Worker {
            window_center_hz,
            window_rate_hz,
            cfg,
            chans: Vec::new(),
            status_chans: Vec::new(),
            devices: HashMap::new(),
            scratch: Scratch::default(),
            bursts_out: Vec::new(),
            decodes: 0,
        };
        w.rebuild();
        w
    }

    fn set_window(&mut self, center_hz: f64, rate_hz: f64) {
        if (self.window_center_hz - center_hz).abs() < 1.0
            && (self.window_rate_hz - rate_hz).abs() < 1.0
        {
            return;
        }
        self.window_center_hz = center_hz;
        self.window_rate_hz = rate_hz;
        self.rebuild();
    }

    fn set_config(&mut self, next: IsmSettings) {
        let families_changed = next.families != self.cfg.families;
        let threshold_changed = next.threshold_db != self.cfg.threshold_db;
        self.cfg = next;
        if families_changed {
            self.rebuild();
        } else if threshold_changed {
            // Not a rebuild: the noise did not move because a slider did, and
            // throwing away every gate's learned floor would blind the decoder
            // for a second every time the operator nudged the threshold.
            for c in &mut self.chans {
                c.gate.set_threshold_db(f32::from(self.cfg.threshold_db));
            }
        }
        self.trim();
    }

    /// Whether any registry protocol on `center_hz` belongs to an enabled family.
    fn any_protocol_on(&self, center_hz: f64) -> bool {
        proto::PROTOCOLS
            .iter()
            .any(|p| (p.channel_hz - center_hz).abs() < 10_000.0 && self.cfg.protocol_enabled(p.id))
    }

    /// Whether any registry protocol exists on `center_hz` at all, enabled or not.
    fn any_protocol_implemented_on(center_hz: f64) -> bool {
        proto::PROTOCOLS.iter().any(|p| (p.channel_hz - center_hz).abs() < 10_000.0)
    }

    fn rebuild(&mut self) {
        self.chans.clear();
        self.status_chans.clear();

        for ch in plan::CHANNELS {
            let in_window = plan::fits(ch, self.window_center_hz, self.window_rate_hz);
            let enabled = self.any_protocol_on(ch.center_hz);

            let reason = if !in_window {
                Some("outside the receiver's window".to_string())
            } else if !Self::any_protocol_implemented_on(ch.center_hz) {
                Some("not decoded yet".to_string())
            } else if !enabled {
                Some("switched off".to_string())
            } else {
                None
            };

            self.status_chans.push(IsmChannelStatus {
                freq_hz: ch.center_hz,
                label: ch.label.to_string(),
                live: reason.is_none(),
                reason,
            });

            if !in_window || !enabled {
                continue;
            }

            let mut ddc = Ddc::new(self.window_rate_hz, ch.rate_hz);
            ddc.set_offset_hz(ch.center_hz - self.window_center_hz);
            let rate = ddc.out_rate();
            self.chans.push(Chan {
                ddc,
                gate: Gate::new(rate, ch.center_hz, f32::from(self.cfg.threshold_db)),
                buf: Vec::new(),
            });
        }
    }

    fn process(&mut self, iq: &[C32]) {
        if self.chans.is_empty() {
            return;
        }
        let now = unix_now();
        // Lifted out of `self` so the per-channel borrow can end before
        // `on_burst`, which needs the worker's scratch and device table.
        let mut bursts = std::mem::take(&mut self.bursts_out);
        for i in 0..self.chans.len() {
            bursts.clear();
            {
                let c = &mut self.chans[i];
                // `Ddc::process` appends, so the scratch has to be cleared.
                c.buf.clear();
                c.ddc.process(iq, &mut c.buf);
                let Chan { gate, buf, .. } = c;
                gate.push(buf, &mut bursts);
            }
            for b in &bursts {
                self.on_burst(b, now);
            }
        }
        self.bursts_out = bursts;
    }

    fn on_burst(&mut self, b: &Burst, now: i64) {
        let cfg = self.cfg;
        let out =
            proto::decode(b, &mut self.scratch, &|p: IsmProtocol| cfg.protocol_enabled(p), true);
        for d in out.decoded {
            self.decodes += 1;
            let id = device_id(d.protocol, &d.device);
            let snr = out.snr_db.round().clamp(-99.0, 99.0) as i16;
            let raw_hex = d.raw_hex;
            match self.devices.get_mut(&id) {
                Some(r) => {
                    r.freq_hz = out.freq_hz;
                    r.snr_db = snr;
                    r.last_at = now;
                    r.count = r.count.saturating_add(1);
                    r.readings = d.readings;
                    r.extra = d.extra;
                    r.model = d.model;
                    r.encrypted = d.encrypted;
                    r.raw_hex = raw_hex;
                }
                None => {
                    // Once per device, not per frame: the first time something is
                    // heard is worth a line on the console, and it is how an
                    // operator confirms the decoder is working without opening
                    // the window.
                    info!(
                        protocol = d.protocol.label(),
                        device = %d.device,
                        freq_hz = out.freq_hz,
                        snr_db = snr,
                        readings = %d
                            .readings
                            .iter()
                            .map(|r| r.fmt_labelled())
                            .collect::<Vec<_>>()
                            .join(", "),
                        "ISM device heard"
                    );
                    self.devices.insert(
                        id,
                        IsmReport {
                            id,
                            protocol: d.protocol,
                            model: d.model,
                            device: d.device,
                            freq_hz: out.freq_hz,
                            snr_db: snr,
                            first_at: now,
                            last_at: now,
                            count: 1,
                            readings: d.readings,
                            extra: d.extra,
                            encrypted: d.encrypted,
                            raw_hex,
                        },
                    );
                }
            }
        }
        self.trim();
    }

    /// Drop the least recently heard devices past the cap.
    fn trim(&mut self) {
        let cap = usize::from(self.cfg.max_devices).max(1);
        while self.devices.len() > cap {
            let Some(&oldest) = self.devices.iter().min_by_key(|(_, r)| r.last_at).map(|(k, _)| k)
            else {
                break;
            };
            self.devices.remove(&oldest);
        }
    }

    /// Newest first, which is the order the panel wants and the order that keeps
    /// a device that has just spoken at the top of a long list.
    fn snapshot(&self) -> Vec<IsmReport> {
        let mut v: Vec<IsmReport> = self.devices.values().cloned().collect();
        v.sort_by(|a, b| b.last_at.cmp(&a.last_at).then(a.device.cmp(&b.device)));
        v
    }

    fn status(&self) -> IsmStatus {
        // Only worth suggesting a retune when the window is what is in the way.
        let window_is_the_problem = self
            .status_chans
            .iter()
            .any(|c| c.reason.as_deref() == Some("outside the receiver's window"));
        IsmStatus {
            channels: self.status_chans.clone(),
            unavailable: self.chans.is_empty().then(|| {
                if window_is_the_problem {
                    "no ISM channel is inside the receiver's window".to_string()
                } else {
                    "no device family is switched on".to_string()
                }
            }),
            suggest_center_hz: window_is_the_problem.then(plan::ideal_center_hz),
            // Every time a gate opened, including the transmissions that ran too
            // long to be one of these protocols. A large count with no decodes is
            // a busy band full of devices sdroxide cannot read yet, and that is
            // worth showing rather than leaving the panel looking broken.
            bursts: self.chans.iter().map(|c| c.gate.opened).sum(),
            decodes: self.decodes,
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// FNV-1a over the protocol and the device string.
///
/// Stable for as long as the process runs, which is all the UI needs, and it
/// costs no dependency. A device that changes its identity — a LaCrosse sensor
/// picks a new random ID when its batteries are replaced — is deliberately a new
/// row: it is, as far as anything on the air can tell, a different sensor.
fn device_id(p: IsmProtocol, device: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    };
    mix(p as u8);
    for b in device.as_bytes() {
        mix(*b);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdroxide_types::IsmFamily;

    fn on() -> IsmSettings {
        IsmSettings {
            enabled: true,
            threshold_db: 12,
            families: IsmFamily::Weather.bit(),
            max_devices: 200,
        }
    }

    /// A window on the ISM band opens the channels that have an enabled decoder
    /// and no others, and says why for each one it skipped.
    #[test]
    fn the_worker_opens_only_the_channels_it_can_use() {
        let w = Worker::new(plan::ideal_center_hz(), 2_025_000.0, on());
        assert_eq!(w.chans.len(), 1, "only the 868.3 MHz channel has a decoder today");
        assert_eq!(w.status_chans.len(), plan::CHANNELS.len());
        let live: Vec<f64> = w.status_chans.iter().filter(|c| c.live).map(|c| c.freq_hz).collect();
        assert_eq!(live, vec![868_300_000.0]);
        for c in w.status_chans.iter().filter(|c| !c.live) {
            assert_eq!(
                c.reason.as_deref(),
                Some("not decoded yet"),
                "{:.3} MHz is in the window, so that is not why it is idle",
                c.freq_hz / 1e6
            );
        }
        assert!(w.status().unavailable.is_none());
        assert!(w.status().suggest_center_hz.is_none());
    }

    /// A receiver tuned away from the band has to say so, and say where to go —
    /// otherwise an empty panel is indistinguishable from a quiet neighbourhood.
    #[test]
    fn a_receiver_off_the_band_reports_where_to_tune() {
        let w = Worker::new(866_000_000.0, 2_025_000.0, on());
        assert!(w.chans.is_empty());
        let st = w.status();
        assert_eq!(
            st.unavailable.as_deref(),
            Some("no ISM channel is inside the receiver's window")
        );
        let want = st.suggest_center_hz.expect("no retune suggested");
        assert!((want - plan::ideal_center_hz()).abs() < 1.0);
        assert!(st.channels.iter().all(|c| !c.live));
    }

    /// Switching every family off is a different problem from being mistuned and
    /// must not be reported as one.
    #[test]
    fn everything_switched_off_is_not_reported_as_mistuned() {
        let mut cfg = on();
        cfg.families = 0;
        let w = Worker::new(plan::ideal_center_hz(), 2_025_000.0, cfg);
        assert!(w.chans.is_empty());
        let st = w.status();
        assert_eq!(st.unavailable.as_deref(), Some("no device family is switched on"));
        assert!(st.suggest_center_hz.is_none(), "the receiver is tuned correctly");
    }

    /// Nudging the threshold must not rebuild the channels: a gate that has to
    /// relearn its floor is a gate that decodes nothing for a second, and the
    /// operator dragging the slider would see the band go dead.
    #[test]
    fn changing_the_threshold_keeps_the_gates() {
        let mut w = Worker::new(plan::ideal_center_hz(), 2_025_000.0, on());
        // Teach the gate a floor.
        let iq = vec![C32::new(0.001, 0.001); 8192];
        for _ in 0..20 {
            w.process(&iq);
        }
        let floor_before = w.chans[0].gate.floor_dbfs();
        assert!(floor_before.is_finite());

        let mut cfg = on();
        cfg.threshold_db = 20;
        w.set_config(cfg);
        assert_eq!(w.chans.len(), 1);
        assert_eq!(w.chans[0].gate.floor_dbfs(), floor_before, "the learned floor was thrown away");
    }

    /// The device table is capped, and it is the *stalest* entry that goes.
    #[test]
    fn the_device_table_evicts_the_least_recently_heard() {
        let mut cfg = on();
        cfg.max_devices = 3;
        let mut w = Worker::new(plan::ideal_center_hz(), 2_025_000.0, cfg);

        for i in 0..5u8 {
            let id = device_id(IsmProtocol::LaCrosseIt, &i.to_string());
            w.devices.insert(
                id,
                IsmReport {
                    id,
                    protocol: IsmProtocol::LaCrosseIt,
                    model: None,
                    device: i.to_string(),
                    freq_hz: 868_300_000.0,
                    snr_db: 20,
                    first_at: i64::from(i),
                    last_at: i64::from(i),
                    count: 1,
                    readings: Vec::new(),
                    extra: Vec::new(),
                    encrypted: false,
                    raw_hex: String::new(),
                },
            );
            w.trim();
        }

        assert_eq!(w.devices.len(), 3);
        let kept: Vec<String> = w.snapshot().into_iter().map(|r| r.device).collect();
        assert_eq!(kept, vec!["4", "3", "2"], "newest first, and the oldest two dropped");
    }

    /// Two different devices must not collide into one row, and the same device
    /// must not split into two.
    #[test]
    fn device_ids_are_stable_and_distinct() {
        let a = device_id(IsmProtocol::LaCrosseIt, "10");
        assert_eq!(a, device_id(IsmProtocol::LaCrosseIt, "10"));
        assert_ne!(a, device_id(IsmProtocol::LaCrosseIt, "11"));
        // The protocol is mixed in as well as the identity, so two protocols
        // that both number their devices from zero cannot collide.
        assert_ne!(
            device_id(IsmProtocol::LaCrosseIt, ""),
            device_id(IsmProtocol::LaCrosseIt, "\0")
        );
    }

    /// Deterministic noise — no `rand` in this tree.
    fn noise_at(seed: &mut u64, sigma: f32) -> C32 {
        let mut u = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            ((*seed >> 11) as f64 / (1u64 << 53) as f64) as f32
        };
        // Box-Muller, so the noise really is Gaussian.
        let (u1, u2) = (u().max(1e-9), u());
        let r = (-2.0 * u1.ln()).sqrt() * sigma;
        C32::new(r * (std::f32::consts::TAU * u2).cos(), r * (std::f32::consts::TAU * u2).sin())
    }

    /// Modulate a byte sequence as the sensor would: preamble, sync word, then
    /// the payload, 2-FSK at `baud`, placed `offset_hz` from the window centre.
    ///
    /// Written from the physical layer's definition — a phase accumulator advanced
    /// at one of two frequencies — and *not* from anything in `demod` or `slice`,
    /// so agreeing with those is evidence rather than a tautology.
    fn modulate(payload: &[u8], rate_hz: f64, offset_hz: f64, amp: f32) -> Vec<C32> {
        let mut bits: Vec<u8> = (0..48).map(|i| (i % 2) as u8).collect();
        for i in (0..16).rev() {
            bits.push(((0x2DD4u32 >> i) & 1) as u8);
        }
        for &b in payload {
            for i in (0..8).rev() {
                bits.push((b >> i) & 1);
            }
        }

        let sps = rate_hz / 17_241.0;
        let dev = 50_000.0;
        let mut out = Vec::new();
        let mut phase = 0.0f64;
        for (i, &b) in bits.iter().enumerate() {
            let f = offset_hz + if b != 0 { dev } else { -dev };
            let n = (((i + 1) as f64 * sps).round() - (i as f64 * sps).round()) as usize;
            for _ in 0..n {
                phase += std::f64::consts::TAU * f / rate_hz;
                out.push(C32::new(amp * phase.cos() as f32, amp * phase.sin() as f32));
            }
        }
        out
    }

    /// The whole stack, end to end: a modulated LaCrosse frame in window-rate IQ
    /// on one side, a device with its readings on the other.
    ///
    /// Every unit test above checks one stage against its own inputs. This is the
    /// only one that checks they are wired together — that the channel `Ddc` lands
    /// the signal where the gate expects it, that the gate hands over enough of
    /// the preamble for the slicers, and that the measured carrier offset is what
    /// the slicing level is taken from.
    #[test]
    fn a_modulated_frame_becomes_a_device_with_readings() {
        const WINDOW: f64 = 2_025_000.0;
        let center = plan::ideal_center_hz();
        let mut w = Worker::new(center, WINDOW, on());
        let mut seed = 0x1234_5678_9ABC_DEF0u64;

        // Let the gate learn a floor first. Around 100 ms is ample: the tracker's
        // time constant is a couple of milliseconds of channel time.
        let mut quiet = vec![C32::default(); 16_384];
        for _ in 0..16 {
            for z in quiet.iter_mut() {
                *z = noise_at(&mut seed, 0.002);
            }
            w.process(&quiet);
        }
        assert!(w.devices.is_empty(), "noise alone produced a device");

        // The published capture: 24.1 °C, 34 %RH, new battery, sensor id 26.
        let mut burst =
            modulate(&[0x96, 0xA6, 0x41, 0x22, 0x50], WINDOW, 868_300_000.0 - center, 0.05);
        for z in burst.iter_mut() {
            *z += noise_at(&mut seed, 0.002);
        }
        w.process(&burst);
        // Trailing quiet so the gate shuts and the burst is delivered.
        for _ in 0..4 {
            for z in quiet.iter_mut() {
                *z = noise_at(&mut seed, 0.002);
            }
            w.process(&quiet);
        }

        let reports = w.snapshot();
        assert_eq!(reports.len(), 1, "expected one device, got {}", reports.len());
        let r = &reports[0];
        assert_eq!(r.protocol, IsmProtocol::LaCrosseIt);
        assert_eq!(r.device, "26");
        assert_eq!(r.count, 1);
        assert_eq!(r.readings.len(), 2, "readings were {:?}", r.readings);
        assert!((r.readings[0].value - 24.1).abs() < 1e-9, "temp {}", r.readings[0].value);
        assert!((r.readings[1].value - 34.0).abs() < 1e-9, "humidity {}", r.readings[1].value);
        assert_eq!(r.extra, vec![("battery".to_string(), "new".to_string())]);
        assert_eq!(r.raw_hex, "96 a6 41 22 50", "the raw frame was not carried through");

        // The frequency has to come from the *signal*, not from the channel it was
        // found on: every device in the band would otherwise be reported on the
        // same four frequencies.
        assert!(
            (r.freq_hz - 868_300_000.0).abs() < 15_000.0,
            "reported {:.4} MHz for a transmitter on 868.3000",
            r.freq_hz / 1e6
        );
        assert!(r.snr_db > 12, "SNR came out {} dB", r.snr_db);

        // Heard twice, it is the same row with a higher count — not a second one.
        w.process(&burst);
        for _ in 0..4 {
            for z in quiet.iter_mut() {
                *z = noise_at(&mut seed, 0.002);
            }
            w.process(&quiet);
        }
        let reports = w.snapshot();
        assert_eq!(reports.len(), 1, "the same sensor produced a second row");
        assert_eq!(reports[0].count, 2);
        assert_eq!(reports[0].id, r.id, "the row's identity changed between frames");
    }

    /// Feeding the worker noise must not produce devices — the whole stack from
    /// gate to CRC has to hold together, not just its pieces.
    #[test]
    fn noise_through_the_whole_worker_produces_no_devices() {
        let mut w = Worker::new(plan::ideal_center_hz(), 2_025_000.0, on());
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut buf = vec![C32::default(); 8192];
        for _ in 0..120 {
            for z in buf.iter_mut() {
                let mut r = || {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    ((seed >> 11) as f64 / (1u64 << 53) as f64) as f32 - 0.5
                };
                *z = C32::new(r() * 0.02, r() * 0.02);
            }
            w.process(&buf);
        }
        assert!(w.devices.is_empty(), "noise produced {} devices", w.devices.len());
    }
}
