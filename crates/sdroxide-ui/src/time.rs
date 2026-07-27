//! Wall clock, on both targets.
//!
//! `SystemTime::now()` panics on `wasm32-unknown-unknown` — there is no clock
//! behind it — so the browser build asks JavaScript instead. Every part of the
//! UI that needs the date rather than a frame time goes through here: the
//! logbook, the waterfall's time gridlines, and the solar view, whose whole
//! scene is a function of an explicit timestamp.

/// Current Unix time (UTC seconds).
pub fn now_unix() -> i64 {
    now_unix_f64() as i64
}

/// Current Unix time as fractional UTC seconds.
pub fn now_unix_f64() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
}
