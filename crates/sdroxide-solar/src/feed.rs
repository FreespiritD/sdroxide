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

use crate::cache::{Cache, Validators};
use crate::donki::{self, CmeEvent, FlareEvent};
use crate::imagery::{self, SdoChannel, SunImage};
use crate::indices::{self, SpaceWeather};
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
/// Failure backoff bounds.
const BACKOFF_BASE: i64 = 30;
const BACKOFF_MAX: i64 = 1800;

/// The things this feed fetches, each on its own cadence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Sun,
    Cme,
    Flare,
    Regions,
    /// 10.7 cm solar flux.
    Flux,
    /// Planetary K and A indices.
    Kp,
    /// Current GOES soft X-ray class.
    Xray,
    /// Ionosonde soundings, for the MUF estimate.
    Muf,
}

impl Source {
    pub const ALL: [Source; 8] = [
        Source::Sun,
        Source::Cme,
        Source::Flare,
        Source::Regions,
        Source::Flux,
        Source::Kp,
        Source::Xray,
        Source::Muf,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Source::Sun => "SDO",
            Source::Cme => "CME",
            Source::Flare => "FLR",
            Source::Regions => "SWPC",
            Source::Flux => "F10.7",
            Source::Kp => "Kp",
            Source::Xray => "XRAY",
            Source::Muf => "MUF",
        }
    }

    /// Refresh cadence, seconds.
    fn period(self) -> i64 {
        match self {
            Source::Sun => 600,      // browse images update every few minutes
            Source::Cme => 1200,
            Source::Flare => 1200,
            Source::Regions => 3600, // a once-a-day product
            Source::Flux => 3600,    // published a few times a day
            Source::Kp => 900,       // three-hourly, but revised in between
            Source::Xray => 300,     // a flare develops in minutes
            Source::Muf => 900,      // ionosondes sound every 5-15 minutes
        }
    }

    /// A fixed stagger, so several never fall due on the same tick. Constant
    /// rather than random: reproducible, and it achieves the same thing.
    fn stagger(self) -> i64 {
        match self {
            Source::Sun => 0,
            Source::Cme => 7,
            Source::Flare => 13,
            Source::Regions => 23,
            Source::Flux => 31,
            Source::Kp => 41,
            Source::Xray => 53,
            Source::Muf => 61,
        }
    }

    fn index(self) -> usize {
        Source::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Per-source freshness, so the overlay can say "cached 4 h ago" instead of
/// presenting stale data as current.
#[derive(Clone, Debug, Default)]
pub struct SourceStatus {
    pub last_ok_unix: i64,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    /// Last time a fetch was *attempted*, successful or not. Failures advance
    /// this even though they do not advance `last_ok_unix`, so a source that is
    /// down is not retried on every fifteen-second tick.
    ///
    /// `None`, not a zero sentinel: the epoch is a perfectly valid timestamp,
    /// and conflating the two makes "never attempted" and "attempted in 1970"
    /// indistinguishable.
    attempt_unix: Option<i64>,
}

impl SourceStatus {
    /// When this source may next be tried, given its cadence and any backoff.
    fn next_due(&self, src: Source) -> Option<i64> {
        let last = self.attempt_unix?;
        let backoff = if self.consecutive_failures == 0 {
            0
        } else {
            (BACKOFF_BASE << (self.consecutive_failures - 1).min(6)).min(BACKOFF_MAX)
        };
        Some(last + src.period().max(backoff) + src.stagger())
    }

    /// Whether this source should be fetched now.
    fn is_due(&self, src: Source, now: i64) -> bool {
        self.next_due(src).is_none_or(|due| due <= now)
    }

    fn record_ok(&mut self, now: i64) {
        self.last_ok_unix = now;
        self.attempt_unix = Some(now);
        self.last_error = None;
        self.consecutive_failures = 0;
    }

    fn record_err(&mut self, now: i64, msg: String) {
        self.attempt_unix = Some(now);
        self.last_error = Some(msg);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// Age of the newest successful fetch, or `None` if there has never been one.
    pub fn age_secs(&self, now_unix: i64) -> Option<i64> {
        (self.last_ok_unix > 0).then(|| (now_unix - self.last_ok_unix).max(0))
    }
}

/// The snapshot the UI reads.
#[derive(Default)]
pub struct SolarData {
    pub cmes: Vec<CmeEvent>,
    pub flares: Vec<FlareEvent>,
    pub regions: Vec<ActiveRegion>,
    /// Shared by `Arc` so handing it to the renderer is a refcount bump, not a
    /// copy of several megabytes.
    pub sun: Option<Arc<SunImage>>,
    /// Bumped on every new image, so the GPU uploads once per image.
    pub sun_gen: u64,
    /// The propagation numbers: flux, K/A, X-ray level and ionosonde soundings.
    pub weather: SpaceWeather,
    pub status: [SourceStatus; Source::ALL.len()],
}

impl SolarData {
    pub fn status(&self, src: Source) -> &SourceStatus {
        &self.status[src.index()]
    }

    /// True once anything at all has been loaded, from network or cache.
    pub fn has_any(&self) -> bool {
        self.sun.is_some() || !self.cmes.is_empty() || !self.regions.is_empty()
    }

    /// The newest successful fetch across every source, for a single "data as
    /// of" readout.
    pub fn newest_ok_unix(&self) -> i64 {
        self.status.iter().map(|s| s.last_ok_unix).max().unwrap_or(0)
    }
}

pub enum FeedCmd {
    RefreshAll,
    SetChannel(SdoChannel),
    SetResolution(u32),
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
        let shared = Arc::new(Mutex::new(SolarData::default()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("solar-feed".into())
            .spawn(move || worker(worker_shared, rx, channel, resolution, wake))
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
) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("sdroxide/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut cache = Cache::open();

    // Publish whatever is on disk before touching the network, so the window
    // has content the moment it opens — including with no connection at all.
    load_cached(&shared, &cache, channel, resolution);
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
                changed |= refresh(src, &agent, &mut cache, &shared, channel, resolution);
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
                load_cached_sun(&shared, &cache, channel, resolution);
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
) {
    let cmes = cache.read_string("cme.json").and_then(|s| donki::parse_cmes(&s).ok());
    let flares = cache.read_string("flr.json").and_then(|s| donki::parse_flares(&s).ok());
    let regions = cache.read_string("regions.json").and_then(|s| swpc::parse_regions(&s).ok());
    let flux = cache.read_string("flux.json").and_then(|s| indices::parse_flux(&s));
    let kp = cache.read_string("kp.json").and_then(|s| indices::parse_kp(&s));
    let xray = cache.read_string("xray.json").and_then(|s| indices::parse_xray(&s));
    let sondes = cache.read_string("ionosondes.json").map(|s| indices::parse_ionosondes(&s));
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
    }
    load_cached_sun(shared, cache, channel, resolution);
}

fn load_cached_sun(
    shared: &Mutex<SolarData>,
    cache: &Cache,
    channel: SdoChannel,
    resolution: u32,
) {
    let name = channel.cache_name(resolution);
    // Decode outside the lock: this is the expensive step.
    let Some(img) = cache
        .read(&name)
        .and_then(|b| imagery::decode(&b, channel, cache.fetched_at(&channel.url(resolution))))
    else {
        return;
    };
    let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
    d.sun = Some(Arc::new(img));
    d.sun_gen += 1;
}

/// Fetch one source. Returns whether anything the UI shows changed.
fn refresh(
    src: Source,
    agent: &ureq::Agent,
    cache: &mut Cache,
    shared: &Mutex<SolarData>,
    channel: SdoChannel,
    resolution: u32,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_never_fetched_source_is_due_immediately() {
        let s = SourceStatus::default();
        for src in Source::ALL {
            assert_eq!(s.next_due(src), None, "{src:?} has a due time before any attempt");
            assert!(s.is_due(src, 0), "{src:?} not due at the epoch");
            assert!(s.is_due(src, 1_784_937_600), "{src:?} not due now");
        }
    }

    #[test]
    fn a_healthy_source_waits_its_cadence() {
        let mut s = SourceStatus::default();
        s.record_ok(1_000_000);
        assert_eq!(s.next_due(Source::Sun), Some(1_000_000 + 600));
        assert_eq!(s.next_due(Source::Regions), Some(1_000_000 + 3600 + 23));
        assert!(!s.is_due(Source::Sun, 1_000_100));
        assert!(s.is_due(Source::Sun, 1_000_700));
        assert_eq!(s.age_secs(1_000_100), Some(100));
    }

    /// A source that is down must back off, or a disconnected machine retries
    /// four URLs every fifteen seconds forever.
    #[test]
    fn failures_back_off_and_then_level_out() {
        let mut s = SourceStatus::default();
        let mut prev = 0;
        for attempt in 1..=10 {
            s.record_err(1_000_000, "boom".into());
            let wait = s.next_due(Source::Sun).unwrap() - 1_000_000 - Source::Sun.stagger();
            assert!(wait >= prev, "backoff went backwards at attempt {attempt}");
            assert!(wait <= BACKOFF_MAX, "backoff {wait} exceeded the cap");
            prev = wait;
        }
        assert_eq!(prev, BACKOFF_MAX, "backoff never reached its cap");
        assert_eq!(s.consecutive_failures, 10);
        assert!(s.last_error.is_some());

        // One success clears it.
        s.record_ok(1_000_100);
        assert_eq!(s.consecutive_failures, 0);
        assert!(s.last_error.is_none());
        assert_eq!(s.next_due(Source::Sun), Some(1_000_100 + 600));
    }

    /// With eight sources on a fifteen-second tick, two falling due together
    /// means two requests in the same instant; the staggers exist to prevent it.
    #[test]
    fn no_two_sources_ever_fall_due_together() {
        let mut s = SourceStatus::default();
        s.record_ok(0);
        let dues: Vec<i64> = Source::ALL.iter().filter_map(|src| s.next_due(*src)).collect();
        assert_eq!(dues.len(), Source::ALL.len());
        let unique: std::collections::HashSet<_> = dues.iter().collect();
        assert_eq!(unique.len(), dues.len(), "sources collide: {dues:?}");
    }

    #[test]
    fn every_source_has_a_distinct_label_and_a_sane_cadence() {
        let labels: std::collections::HashSet<_> =
            Source::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), Source::ALL.len(), "duplicate source labels");
        for src in Source::ALL {
            // Nothing polled more than once a minute, nothing left over a day.
            assert!((60..=86_400).contains(&src.period()), "{src:?} period {}", src.period());
            assert_eq!(Source::ALL[src.index()], src);
        }
    }

    #[test]
    fn an_empty_snapshot_reports_itself_empty() {
        let d = SolarData::default();
        assert!(!d.has_any());
        assert_eq!(d.status(Source::Cme).last_ok_unix, 0);
        assert!(d.status(Source::Sun).last_error.is_none());
        assert_eq!(d.status(Source::Sun).age_secs(1_784_937_600), None);
    }
}
