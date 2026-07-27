//! The background data feed: one worker thread that fetches DONKI, NOAA SWPC
//! and SDO, and publishes a snapshot the UI reads under a short lock.
//!
//! Rules this is built around:
//!
//! * **The UI thread never touches the network.** Not a blocking call, not a
//!   JPEG decode — a 2048² decode is 150 ms, which is ten dropped frames.
//! * **The lock is never held across I/O.** The worker fetches and decodes
//!   first, then takes the lock only to swap a field.
//! * **Network happens only while the window is open.** The thread is started
//!   on first open and stops when the handle is dropped; a session that never
//!   opens the view makes no outbound connection at all.
//! * **Offline is a supported state, not an error.** The disk cache is
//!   published before the first request, and a failing source keeps serving its
//!   cached value with an honest age next to it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::aurora::{self, AuroraOval, HemisphericPower, KpPoint};
use crate::cache::{Cache, Validators};
use crate::data::{SolarData, Source, SourceStatus};
use crate::donki::{self, CmeEvent, FlareEvent};
use crate::imagery::{self, SdoChannel, SunImage};
use crate::indices::{self};
use crate::satellites::{self, Satellite};
use crate::swpc::{self, ActiveRegion};

/// How far back events are requested, days.
const WINDOW_DAYS: i64 = 30;
/// Body size caps. The 4096 solar images are ~2 MB; the rest are far smaller.
const JSON_LIMIT: u64 = 16 * 1024 * 1024;
const IMAGE_LIMIT: u64 = 24 * 1024 * 1024;
/// How long a request may take in total.
const TIMEOUT: Duration = Duration::from_secs(25);
/// Worker wake interval; individual sources have their own, longer, cadences.
const TICK: Duration = Duration::from_secs(15);

pub enum FeedCmd {
    RefreshAll,
    SetChannel(SdoChannel),
    SetResolution(u32),
}

/// A payload in the form it arrived in, for a relay that has to hand the same
/// bytes to somebody else.
///
/// [`SolarData`] holds *decoded* products — RGBA pixels and SGP4 constants —
/// which are both far larger than the original and not serialisable. The
/// server's browser relay needs the original JPEG and the original element set
/// instead, so the worker offers them here rather than making anyone re-encode
/// or re-fetch. Only sources whose wire form is the raw bytes appear.
pub enum RawUpdate {
    Sun { channel: SdoChannel, fetched_unix: i64, jpeg: Vec<u8> },
    /// An element set: `geo` distinguishes QO-100 from the amateur list.
    Tle { geo: bool, text: String },
}

/// Handle to the worker. Dropping it stops the thread, which is what confines
/// network activity to the window's lifetime.
pub struct SolarFeed {
    shared: Arc<Mutex<SolarData>>,
    tx: Sender<FeedCmd>,
}

impl SolarFeed {
    /// Start the worker. `wake` is called after every published change — the UI
    /// passes a closure that repaints the solar viewport.
    pub fn start(
        channel: SdoChannel,
        resolution: u32,
        wake: impl Fn() + Send + 'static,
    ) -> SolarFeed {
        SolarFeed::start_with_raw(channel, resolution, wake, None)
    }

    /// As [`SolarFeed::start`], but also publishing raw payloads to `raw_tx`.
    ///
    /// The channel must be unbounded or drained promptly: the worker never
    /// blocks on it, and a send that fails is simply dropped, so a stalled
    /// consumer costs freshness rather than the whole feed.
    pub fn start_with_raw(
        channel: SdoChannel,
        resolution: u32,
        wake: impl Fn() + Send + 'static,
        raw_tx: Option<Sender<RawUpdate>>,
    ) -> SolarFeed {
        let shared = Arc::new(Mutex::new(SolarData::default()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("solar-feed".into())
            .spawn(move || worker(worker_shared, rx, channel, resolution, wake, raw_tx))
            .map_err(|e| tracing::error!("could not start the solar feed thread: {e}"))
            .ok();
        SolarFeed { shared, tx }
    }

    pub fn data(&self) -> std::sync::MutexGuard<'_, SolarData> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A second handle on the snapshot, for code that cannot borrow the feed
    /// (the window's deferred render closure, which outlives any borrow).
    pub fn shared(&self) -> Arc<Mutex<SolarData>> {
        Arc::clone(&self.shared)
    }

    pub fn send(&self, cmd: FeedCmd) {
        // A full or closed channel means the worker is gone; nothing to do.
        let _ = self.tx.send(cmd);
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn worker(
    shared: Arc<Mutex<SolarData>>,
    rx: Receiver<FeedCmd>,
    mut channel: SdoChannel,
    mut resolution: u32,
    wake: impl Fn(),
    raw_tx: Option<Sender<RawUpdate>>,
) {
    // Never blocks the fetch loop: a relay that has stopped draining costs
    // freshness, not the feed.
    let raw = |u: RawUpdate| {
        if let Some(tx) = &raw_tx {
            let _ = tx.try_send(u);
        }
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("sdroxide/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut cache = Cache::open();

    // Publish whatever is on disk before touching the network, so the window
    // has content the moment it opens — including with no connection at all.
    load_cached(&shared, &cache, channel, resolution, &raw);
    wake();

    loop {
        let now = now_unix();
        let mut changed = false;
        for src in Source::ALL {
            let due = {
                let d = shared.lock().unwrap_or_else(|e| e.into_inner());
                d.status(src).is_due(src, now)
            };
            if due {
                changed |= refresh(src, &agent, &mut cache, &shared, channel, resolution, &raw);
            }
        }
        if changed {
            wake();
        }

        match rx.recv_timeout(TICK) {
            Ok(FeedCmd::RefreshAll) => {
                let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
                // Clear the schedule so every source is due on the next
                // pass, but keep `last_ok_unix` so the age readouts stay honest
                // until the new data actually lands.
                for s in &mut d.status {
                    s.attempt_unix = None;
                    s.consecutive_failures = 0;
                }
            }
            Ok(FeedCmd::SetChannel(c)) => {
                channel = c;
                // Serve the cached image for the new channel immediately, then
                // let the normal cadence refresh it.
                load_cached_sun(&shared, &cache, channel, resolution, &raw);
                let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
                d.status[Source::Sun.index()] = SourceStatus::default();
                drop(d);
                wake();
            }
            Ok(FeedCmd::SetResolution(r)) => {
                resolution = r.clamp(512, 4096);
                let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
                d.status[Source::Sun.index()] = SourceStatus::default();
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The handle was dropped: the window closed, so stop fetching.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    tracing::debug!("solar feed thread stopped");
}

fn load_cached(
    shared: &Mutex<SolarData>,
    cache: &Cache,
    channel: SdoChannel,
    resolution: u32,
    raw: &dyn Fn(RawUpdate),
) {
    let cmes = cache.read_string("cme.json").and_then(|s| donki::parse_cmes(&s).ok());
    let flares = cache.read_string("flr.json").and_then(|s| donki::parse_flares(&s).ok());
    let regions = cache.read_string("regions.json").and_then(|s| swpc::parse_regions(&s).ok());
    let flux = cache.read_string("flux.json").and_then(|s| indices::parse_flux(&s));
    let kp = cache.read_string("kp.json").and_then(|s| indices::parse_kp(&s));
    let xray = cache.read_string("xray.json").and_then(|s| indices::parse_xray(&s));
    let sondes = cache.read_string("ionosondes.json").map(|s| indices::parse_ionosondes(&s));
    // Keep the element sets in their original form as well as parsed: a relay
    // forwards the text, and re-serialising SGP4 constants is not possible.
    let sats_txt = cache.read_string("amateur.txt");
    let sats_geo_txt = cache.read_string("qo100.txt");
    let sats = sats_txt.as_deref().map(satellites::parse_tles);
    let sats_geo = sats_geo_txt.as_deref().map(satellites::parse_tles);
    let oval = cache.read_string("ovation.json").and_then(|s| aurora::parse_ovation(&s));
    let power = cache
        .read_string("hemipower.txt")
        .and_then(|s| aurora::parse_hemispheric_power(&s));
    let kp_forecast = cache.read_string("kpforecast.json").map(|s| aurora::parse_kp_forecast(&s));
    {
        let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = cmes {
            d.cmes = v;
        }
        if let Some(v) = flares {
            d.flares = v;
        }
        if let Some(v) = regions {
            d.regions = v;
        }
        d.weather.flux = flux;
        d.weather.geomagnetic = kp;
        d.weather.xray = xray;
        if let Some(v) = sondes {
            d.weather.ionosondes = v;
        }
        if let Some(v) = sats {
            d.sats_amateur = v;
        }
        if let Some(v) = sats_geo {
            d.sats_geo = v;
        }
        if let Some(v) = oval {
            d.aurora = Some(Arc::new(v));
            d.aurora_gen += 1;
        }
        d.aurora_power = power;
        if let Some(v) = kp_forecast {
            d.kp_forecast = v;
        }
    }
    if let Some(text) = sats_txt {
        raw(RawUpdate::Tle { geo: false, text });
    }
    if let Some(text) = sats_geo_txt {
        raw(RawUpdate::Tle { geo: true, text });
    }
    load_cached_sun(shared, cache, channel, resolution, raw);
}

fn load_cached_sun(
    shared: &Mutex<SolarData>,
    cache: &Cache,
    channel: SdoChannel,
    resolution: u32,
    raw: &dyn Fn(RawUpdate),
) {
    let name = channel.cache_name(resolution);
    let Some(bytes) = cache.read(&name) else { return };
    let fetched_unix = cache.fetched_at(&channel.url(resolution));
    // Decode outside the lock: this is the expensive step.
    let Some(img) = imagery::decode(&bytes, channel, fetched_unix) else {
        return;
    };
    {
        let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
        d.sun = Some(Arc::new(img));
        d.sun_gen += 1;
    }
    raw(RawUpdate::Sun { channel, fetched_unix, jpeg: bytes });
}

/// Fetch one source. Returns whether anything the UI shows changed.
fn refresh(
    src: Source,
    agent: &ureq::Agent,
    cache: &mut Cache,
    shared: &Mutex<SolarData>,
    channel: SdoChannel,
    resolution: u32,
    raw: &dyn Fn(RawUpdate),
) -> bool {
    let now = now_unix();
    let (url, name, limit) = match src {
        Source::Sun => (channel.url(resolution), channel.cache_name(resolution), IMAGE_LIMIT),
        Source::Cme => (
            donki::cme_url(now - WINDOW_DAYS * 86_400, now + 86_400),
            "cme.json".to_string(),
            JSON_LIMIT,
        ),
        Source::Flare => (
            donki::flare_url(now - WINDOW_DAYS * 86_400, now + 86_400),
            "flr.json".to_string(),
            JSON_LIMIT,
        ),
        Source::Regions => (swpc::REGIONS_URL.to_string(), "regions.json".to_string(), JSON_LIMIT),
        Source::Flux => (indices::FLUX_URL.to_string(), "flux.json".to_string(), JSON_LIMIT),
        Source::Kp => (indices::KP_URL.to_string(), "kp.json".to_string(), JSON_LIMIT),
        Source::Xray => (indices::XRAY_URL.to_string(), "xray.json".to_string(), JSON_LIMIT),
        Source::Muf => (indices::IONOSONDE_URL.to_string(), "ionosondes.json".to_string(), JSON_LIMIT),
        Source::Sats => (satellites::AMATEUR_URL.to_string(), "amateur.txt".to_string(), JSON_LIMIT),
        Source::SatGeo => (satellites::QO100_URL.to_string(), "qo100.txt".to_string(), JSON_LIMIT),
        Source::Aurora => {
            (aurora::OVATION_URL.to_string(), "ovation.json".to_string(), JSON_LIMIT)
        }
        Source::AuroraPower => {
            (aurora::HEMI_POWER_URL.to_string(), "hemipower.txt".to_string(), JSON_LIMIT)
        }
        Source::KpForecast => {
            (aurora::KP_FORECAST_URL.to_string(), "kpforecast.json".to_string(), JSON_LIMIT)
        }
    };

    match http_get(agent, &url, &cache.validators(&url), limit) {
        Ok(None) => {
            // 304: what we already have is current.
            cache.touch(&url, now);
            let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
            d.status[src.index()].record_ok(now);
            false
        }
        Ok(Some((bytes, validators))) => {
            // Parse and decode before taking the lock.
            let parsed = parse(src, &bytes, channel, now);
            let ok = match &parsed {
                Parsed::None => false,
                _ => true,
            };
            if ok {
                cache.write(&name, &url, &bytes, Validators { fetched_unix: now, ..validators });
                // Forward the original bytes for the sources whose wire form is
                // the payload itself, before `bytes` is dropped.
                match src {
                    Source::Sun => raw(RawUpdate::Sun {
                        channel,
                        fetched_unix: now,
                        jpeg: bytes.clone(),
                    }),
                    Source::Sats | Source::SatGeo => {
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            raw(RawUpdate::Tle {
                                geo: src == Source::SatGeo,
                                text: text.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
            match parsed {
                Parsed::Cmes(v) => d.cmes = v,
                Parsed::Flares(v) => d.flares = v,
                Parsed::Regions(v) => d.regions = v,
                Parsed::Sun(img) => {
                    d.sun = Some(Arc::new(img));
                    d.sun_gen += 1;
                }
                Parsed::Flux(v) => d.weather.flux = Some(v),
                Parsed::Kp(v) => d.weather.geomagnetic = Some(v),
                Parsed::Xray(v) => d.weather.xray = Some(v),
                Parsed::Ionosondes(v) => d.weather.ionosondes = v,
                Parsed::Sats(v) => d.sats_amateur = v,
                Parsed::SatGeo(v) => d.sats_geo = v,
                Parsed::Aurora(v) => {
                    d.aurora = Some(Arc::new(v));
                    d.aurora_gen += 1;
                }
                Parsed::AuroraPower(v) => d.aurora_power = Some(v),
                Parsed::KpForecast(v) => d.kp_forecast = v,
                Parsed::None => {
                    d.status[src.index()]
                        .record_err(now, format!("{} returned unusable data", src.label()));
                    return false;
                }
            }
            d.status[src.index()].record_ok(now);
            true
        }
        Err(e) => {
            tracing::warn!("solar feed: {} fetch failed: {e}", src.label());
            let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
            d.status[src.index()].record_err(now, e);
            // The status line changed even though the data did not.
            true
        }
    }
}

enum Parsed {
    Cmes(Vec<CmeEvent>),
    Flares(Vec<FlareEvent>),
    Regions(Vec<ActiveRegion>),
    Sun(SunImage),
    Flux(indices::SolarFlux),
    Kp(indices::GeomagneticIndex),
    Xray(indices::XrayLevel),
    Ionosondes(Vec<indices::Ionosonde>),
    Sats(Vec<Satellite>),
    SatGeo(Vec<Satellite>),
    Aurora(AuroraOval),
    AuroraPower(HemisphericPower),
    KpForecast(Vec<KpPoint>),
    None,
}

fn parse(src: Source, bytes: &[u8], channel: SdoChannel, now: i64) -> Parsed {
    match src {
        Source::Sun => match imagery::decode(bytes, channel, now) {
            Some(img) => Parsed::Sun(img),
            None => Parsed::None,
        },
        _ => {
            let Ok(text) = std::str::from_utf8(bytes) else { return Parsed::None };
            let parsed = match src {
                Source::Cme => donki::parse_cmes(text).map(Parsed::Cmes),
                Source::Flare => donki::parse_flares(text).map(Parsed::Flares),
                Source::Regions => swpc::parse_regions(text).map(Parsed::Regions),
                // These four return an Option rather than a Result: a feed that
                // is momentarily empty is normal, not an error.
                Source::Flux => return indices::parse_flux(text).map_or(Parsed::None, Parsed::Flux),
                Source::Kp => return indices::parse_kp(text).map_or(Parsed::None, Parsed::Kp),
                Source::Xray => return indices::parse_xray(text).map_or(Parsed::None, Parsed::Xray),
                Source::Muf => {
                    let v = indices::parse_ionosondes(text);
                    return if v.is_empty() { Parsed::None } else { Parsed::Ionosondes(v) };
                }
                Source::Sats | Source::SatGeo => {
                    let v = satellites::parse_tles(text);
                    return match (v.is_empty(), src) {
                        (true, _) => Parsed::None,
                        (false, Source::SatGeo) => Parsed::SatGeo(v),
                        (false, _) => Parsed::Sats(v),
                    };
                }
                Source::Aurora => {
                    return aurora::parse_ovation(text).map_or(Parsed::None, Parsed::Aurora);
                }
                Source::AuroraPower => {
                    return aurora::parse_hemispheric_power(text)
                        .map_or(Parsed::None, Parsed::AuroraPower);
                }
                Source::KpForecast => {
                    let v = aurora::parse_kp_forecast(text);
                    return if v.is_empty() { Parsed::None } else { Parsed::KpForecast(v) };
                }
                Source::Sun => unreachable!(),
            };
            parsed.unwrap_or_else(|e| {
                tracing::warn!("solar feed: {} parse failed: {e}", src.label());
                Parsed::None
            })
        }
    }
}

/// Conditional GET. `Ok(None)` means 304 Not Modified.
fn http_get(
    agent: &ureq::Agent,
    url: &str,
    validators: &Validators,
    limit: u64,
) -> Result<Option<(Vec<u8>, Validators)>, String> {
    let mut req = agent.get(url);
    if let Some(etag) = &validators.etag {
        req = req.header("If-None-Match", etag);
    }
    if let Some(lm) = &validators.last_modified {
        req = req.header("If-Modified-Since", lm);
    }

    let mut resp = req.call().map_err(|e| e.to_string())?;
    if resp.status() == 304 {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let header = |name: &str| {
        resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    };
    let next = Validators {
        etag: header("etag"),
        last_modified: header("last-modified"),
        fetched_unix: 0,
    };
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(Some((bytes, next)))
}
