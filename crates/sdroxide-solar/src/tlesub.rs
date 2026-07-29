//! Subscribed element-set listings: TLEs fetched from a URL and kept current.
//!
//! Pasted elements go stale, and SGP4 on a fortnight-old TLE is worth very
//! little — [`crate::satellites::MAX_ELEMENT_AGE_S`] is where this crate stops
//! propagating them at all. Anything the operator means to keep tracking wants
//! a subscription instead of a paste.
//!
//! Two callers, one implementation:
//!
//! * The feed's worker refreshes subscriptions on the same six-hourly cadence
//!   as the amateur element set, while the solar window is open.
//! * The settings dialog fetches them on demand, so a subscription added there
//!   is usable immediately rather than at the next time the window happens to
//!   be open.
//!
//! Both go through the same disk cache as every other source, so a listing
//! fetched by either is served instantly — and offline — to the other.

use sdroxide_types::TleSubscription;

use crate::cache::{Cache, Validators};
use crate::satellites::Satellite;

/// Body cap. The largest CelesTrak group is a few hundred kilobytes; a
/// megabyte is generous and still bounds a misconfigured URL.
const BODY_LIMIT: u64 = 4 * 1024 * 1024;

/// What one subscription's last fetch did, for the settings dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubStatus {
    pub url: String,
    /// When the listing was last fetched successfully, or 0 for never.
    pub fetched_unix: i64,
    /// How many satellites it yielded after the filter.
    pub count: usize,
    /// Why the last attempt failed, if it did. A failure does not clear
    /// `count`: the cached listing is still what is being tracked.
    pub error: Option<String>,
}

/// Cache file name for a subscription's body.
///
/// Keyed by a hash of the URL rather than by the operator's name for it:
/// renaming a subscription must not orphan its cached listing, and two
/// subscriptions cannot be given the same name and collide.
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.trim().hash(&mut h);
    format!("tlesub_{:016x}.txt", h.finish())
}

/// The satellites a subscription is currently tracking, from the disk cache.
///
/// No network: this is what the tracker uses at startup and what it falls back
/// to when a refresh fails.
pub fn cached(cache: &Cache, sub: &TleSubscription) -> Vec<Satellite> {
    match cache.read_string(&cache_name(&sub.url)) {
        Some(text) => parse(sub, &text),
        None => Vec::new(),
    }
}

/// A subscription's cached listing, exactly as it was fetched.
///
/// For the browser relay, which forwards the original text rather than the
/// parsed SGP4 constants — those cannot be serialised.
pub fn cached_text(cache: &Cache, sub: &TleSubscription) -> Option<String> {
    cache.read_string(&cache_name(&sub.url))
}

/// Status of a subscription from the cache alone, so the settings dialog can
/// report on one without opening a socket.
pub fn cached_status(cache: &Cache, sub: &TleSubscription) -> SubStatus {
    SubStatus {
        url: sub.url.clone(),
        fetched_unix: cache.fetched_at(&sub.url),
        count: cached(cache, sub).len(),
        error: None,
    }
}

/// Parse a fetched listing into the satellites this subscription wants.
///
/// Everything that comes through is flagged [`Satellite::custom`] — it is here
/// because the operator asked for it — and `popular` follows the
/// subscription's own orbit-ring setting, which is what decides whether the
/// scene draws a ring and a label for it.
fn parse(sub: &TleSubscription, text: &str) -> Vec<Satellite> {
    let mut v = crate::satellites::parse_subscribed_tles(text);
    v.retain(|s| sub.wants(s.norad_id));
    for s in &mut v {
        // Or-ed, not assigned: the curated few in [`crate::satellites::POPULAR`]
        // keep their orbit rings and labels whatever the subscription says.
        // Turning rings off for a ninety-satellite group must not also turn
        // them off for the ten the view is built around.
        s.popular |= sub.orbits;
    }
    v
}

/// Fetch one subscription, updating the cache. Returns what it now tracks.
///
/// A failed fetch is not a failed subscription: the cached listing is returned
/// with the error beside it, because a day-old TLE and no network still beats
/// an empty sky.
pub fn refresh(
    agent: &ureq::Agent,
    cache: &mut Cache,
    sub: &TleSubscription,
    now_unix: i64,
) -> (Vec<Satellite>, SubStatus) {
    let url = sub.url.trim().to_string();
    let name = cache_name(&url);
    let mut status = SubStatus { url: url.clone(), ..Default::default() };

    match crate::feed::http_get(agent, &url, &cache.validators(&url), BODY_LIMIT) {
        // 304: what is on disk is current.
        Ok(None) => {
            cache.touch(&url, now_unix);
            let sats = cached(cache, sub);
            status.fetched_unix = now_unix;
            status.count = sats.len();
            (sats, status)
        }
        Ok(Some((bytes, validators))) => {
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    status.error = Some("the listing is not text".into());
                    let sats = cached(cache, sub);
                    status.fetched_unix = cache.fetched_at(&url);
                    status.count = sats.len();
                    return (sats, status);
                }
            };
            let sats = parse(sub, &text);
            // An empty parse means the URL served something that is not an
            // element set — an HTML error page is the usual culprit. Keeping
            // the previous body is better than caching the error page.
            if crate::satellites::parse_tles(&text).is_empty() {
                status.error = Some("no element sets in the response".into());
                let old = cached(cache, sub);
                status.fetched_unix = cache.fetched_at(&url);
                status.count = old.len();
                return (old, status);
            }
            cache.write(
                &name,
                &url,
                text.as_bytes(),
                Validators { fetched_unix: now_unix, ..validators },
            );
            status.fetched_unix = now_unix;
            status.count = sats.len();
            (sats, status)
        }
        Err(e) => {
            tracing::warn!("TLE subscription {url}: {e}");
            let sats = cached(cache, sub);
            status.error = Some(e);
            status.fetched_unix = cache.fetched_at(&url);
            status.count = sats.len();
            (sats, status)
        }
    }
}

/// Fetch every enabled subscription. Used by the settings dialog's "Update
/// now", which has no feed thread to ask.
///
/// Blocking, and up to one HTTPS round trip per subscription — call it off the
/// paint path, the way the HPSDR discovery button does.
pub fn refresh_all(subs: &[TleSubscription]) -> Vec<SubStatus> {
    let agent = agent();
    let mut cache = Cache::open();
    let now = crate::feed::now_unix();
    subs.iter()
        .filter(|s| s.enabled && s.is_valid())
        .map(|s| refresh(&agent, &mut cache, s, now).1)
        .collect()
}

/// Status of every subscription from the disk cache alone — no network.
pub fn status_all(subs: &[TleSubscription]) -> Vec<SubStatus> {
    let cache = Cache::open();
    subs.iter().map(|s| cached_status(&cache, s)).collect()
}

/// An HTTP agent with the same timeout and user agent as the feed's.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(25)))
        .user_agent(concat!("sdroxide/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMATEUR: &str = include_str!("../tests/fixtures/amateur.txt");

    fn sub() -> TleSubscription {
        TleSubscription::new("Test", "https://example.invalid/tle.txt")
    }

    /// A group subscription tracks the whole listing, but only the curated few
    /// are ringed and labelled — ninety rings at once is unreadable, and the
    /// rest stay dots behind ALL SATS exactly as they did when the amateur set
    /// was a fixed source.
    #[test]
    fn a_subscription_tracks_everything_in_the_listing_by_default() {
        let v = parse(&sub(), AMATEUR);
        assert!(v.len() > 80, "only {} satellites", v.len());
        // Operator-supplied, so they are visible without ALL SATS...
        assert!(v.iter().all(|s| s.custom));
        // ...and only the curated few are ringed and labelled, because ninety
        // rings is unreadable. That the ISS keeps its ring with the
        // subscription's own setting off is the whole point of the or.
        assert!(v.iter().any(|s| s.popular), "the curated satellites lost their rings");
        assert!(v.iter().filter(|s| s.popular).count() < 15, "too many were ringed");
        assert!(v.iter().find(|s| s.norad_id == 25544).is_some_and(|s| s.popular));
    }

    #[test]
    fn a_filter_narrows_it_to_the_listed_catalogue_numbers() {
        let mut s = sub();
        s.only = vec![25544, 43700];
        s.orbits = true;
        let v = parse(&s, AMATEUR);
        assert_eq!(v.len(), 2, "the filter let something else through");
        let mut ids: Vec<u64> = v.iter().map(|s| s.norad_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![25544, 43700]);
        // A short, deliberately chosen list is worth ringing and labelling.
        assert!(v.iter().all(|s| s.popular && s.custom));
        // A catalogue number the listing does not have is simply absent rather
        // than an error — a group's membership changes under you.
        let mut s = sub();
        s.only = vec![999_999];
        assert!(parse(&s, AMATEUR).is_empty());
    }

    /// The filter box takes whatever the operator types at it.
    #[test]
    fn the_filter_box_round_trips_through_its_text() {
        let mut s = sub();
        s.set_only_text("25544, 43700 33591");
        assert_eq!(s.only, vec![25544, 43700, 33591]);
        assert_eq!(s.only_text(), "25544, 43700, 33591");
        // Junk is dropped rather than clearing the filter or failing.
        s.set_only_text("25544, , oops, 7530,");
        assert_eq!(s.only, vec![25544, 7530]);
        // Emptied means "everything" again.
        s.set_only_text("   ");
        assert!(s.only.is_empty());
        assert!(s.wants(1) && s.wants(2));
    }

    #[test]
    fn a_subscription_says_what_is_wrong_with_it() {
        assert_eq!(sub().problem(), None);
        assert_eq!(TleSubscription::new("", "https://x/y").problem(), Some("no name"));
        assert_eq!(TleSubscription::new("n", "").problem(), Some("no URL"));
        // Element sets fetched in clear would put both the data and the fact of
        // the request on the wire.
        assert_eq!(
            TleSubscription::new("n", "http://x/y").problem(),
            Some("the URL must be https://")
        );
    }

    /// Renaming a subscription must not orphan its cached listing, and two
    /// different URLs must not land on the same file.
    #[test]
    fn the_cache_name_follows_the_url_and_nothing_else() {
        let a = cache_name("https://celestrak.org/x?GROUP=weather");
        assert_eq!(a, cache_name(" https://celestrak.org/x?GROUP=weather "));
        assert_ne!(a, cache_name("https://celestrak.org/x?GROUP=cubesat"));
        // It ends up in a path, so it has to stay a plain file name.
        assert!(!a.contains('/') && !a.contains('.') || a.ends_with(".txt"));
        assert!(a.starts_with("tlesub_") && a.ends_with(".txt"));
    }

    /// Every offered group has to be a fetchable https URL with a distinct
    /// address, and the two that are on by default have to be the ones the
    /// tracker used to fetch by itself — otherwise a fresh install comes up
    /// with an empty sky.
    #[test]
    fn the_offered_celestrak_groups_are_usable() {
        use sdroxide_types::CELESTRAK_GROUPS;
        let mut urls: Vec<&str> = Vec::new();
        for g in CELESTRAK_GROUPS {
            assert!(!g.name.is_empty() && !g.hint.is_empty(), "{} is missing its text", g.name);
            assert_eq!(
                TleSubscription::new(g.name, g.url).problem(),
                None,
                "{}: {}",
                g.name,
                g.url
            );
            urls.push(g.url);
        }
        let n = urls.len();
        urls.sort_unstable();
        urls.dedup();
        assert_eq!(urls.len(), n, "two groups share a URL");

        let on: Vec<&str> =
            CELESTRAK_GROUPS.iter().filter(|g| g.default_on).map(|g| g.name).collect();
        assert_eq!(on, vec!["Amateur radio", "ISS"]);
        // The amateur one has to be the listing the tracker used to fetch, or a
        // fresh install would quietly track a different set of satellites.
        let amateur = CELESTRAK_GROUPS.iter().find(|g| g.name == "Amateur radio").unwrap();
        assert_eq!(amateur.url, crate::satellites::AMATEUR_URL);
        assert!(!amateur.orbits, "ninety orbit rings at once is unreadable");
        // The ISS is one satellite, so it is worth a ring and a label.
        let iss = CELESTRAK_GROUPS.iter().find(|g| g.name == "ISS").unwrap();
        assert!(iss.url.contains("CATNR=25544"));
        assert!(iss.orbits);
    }
}
