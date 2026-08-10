//! What the solar-system 3D view takes from the main window.
//!
//! The 3D view runs in its own viewport with its own state (see
//! [`crate::solar3d`]) and cannot borrow [`SdroxideApp`], so the traffic layer
//! it draws — who is being heard on the digital modes right now — is built
//! here each frame and handed over by value.
//!
//! Band conditions go the other way, and are here for that reason: they are the
//! one piece of space weather the *main* window shows, so the main window keeps
//! them current itself rather than waiting for the 3D view to be opened.

use eframe::egui;

// The traffic layer only exists where the 3D window does.
#[cfg(not(target_arch = "wasm32"))]
use crate::time::now_unix;

use crate::app::{SdroxideApp, persist};

impl SdroxideApp {
    /// The FT8/FT4 activity to plot on the 3D globe: the same decoded stations
    /// the flat map shows, plus the station being worked.
    ///
    /// Read-only — the decode bookkeeping stays in `qso_area`, which is the one
    /// place that knows a decode is new. Outside the digital modes the map
    /// simply ages out and empties.
    #[cfg(not(target_arch = "wasm32"))]
    pub(in crate::app) fn digi_traffic(&self, now_t: f64) -> crate::solar3d::DigiTraffic {
        let status = self.digi_status.as_ref();
        let mut traffic = self.digi_stations.traffic(
            now_t,
            status.and_then(|s| s.dx_grid.as_deref()),
            self.digi_preview.as_ref().map(|(_, ll)| *ll),
            status.is_some_and(|s| s.transmitting),
        );
        // JS8 has no QSO sequencer to ask for `dx_grid`, because a chat has no
        // Tx1–Tx6 to be part-way through. What an operator means by "in a QSO"
        // there is the station the composer is aimed at, so that is what gets
        // the arc — highlighted exactly as an FT8 contact in progress is.
        if self.state.rx[0].mode.is_js8() {
            let heard =
                status.and_then(|s| s.js8.as_ref()).map(|j| j.heard.as_slice()).unwrap_or(&[]);
            traffic.dx = self
                .js8_grid_for(&self.js8_target, heard)
                .as_deref()
                .and_then(sdroxide_types::grid_to_latlon);
        }
        // Weather fax has no callsign and no grid to place a station by, but it
        // does have a transmitter with a known location — so the chart being
        // received gets the same path across the globe a QSO would, which turns
        // an anonymous picture into "this came 900 km over the North Sea".
        if self.state.rx[0].mode.is_wefax()
            && let Some((st, _)) = sdroxide_types::WefaxStation::at_dial(self.state.rx_freq_hz())
        {
            traffic.dx = Some((st.lat, st.lon));
            traffic.dx_label = Some(st.name.to_string());
        }
        // A broadcast station is the same case again: no callsign, but a known
        // transmitter site, so tuning one draws the path the signal actually
        // travelled. Only when nothing else has claimed the arc — a QSO in
        // progress outranks whatever the dial happens to be sitting on — and
        // deliberately not gated on AM, because plenty of shortwave listening is
        // done in ECSS on one sideband.
        if traffic.dx.is_none()
            && let Some((st, lat, lon)) = sdroxide_types::broadcast::at_dial(
                &self.broadcast,
                self.state.rx_freq_hz(),
                now_unix(),
            )
            .and_then(|st| Some((st, st.lat?, st.lon?)))
        {
            traffic.dx = Some((lat, lon));
            traffic.dx_label = Some(if st.site.is_empty() {
                st.name.clone()
            } else {
                format!("{} · {}", st.name, st.site)
            });
        }
        traffic
    }

    /// Keep the band-conditions verdicts and the day/night flag current.
    ///
    /// Called once a frame, and cheap on almost all of them: the fetch is a
    /// worker thread polled without blocking, and the ephemeris runs once a
    /// minute rather than sixty times a second for an answer that changes twice
    /// a day.
    pub(in crate::app) fn refresh_band_conditions(&mut self, now_t: f64) {
        // A fetch that finished. `None` on the channel means the worker had
        // nothing usable — an expired cache and an unreachable server both
        // land here — so the last known verdicts stay up, with their age.
        if let Some(rx) = &self.band_conditions_fetch
            && let Ok(result) = rx.try_recv()
        {
            self.band_conditions_fetch = None;
            if result.is_some() {
                self.band_conditions = result;
            }
        }
        // Ask again on the hour. The worker returns the cached copy without a
        // request when it is still current, so this interval is when to *look*
        // rather than when to fetch — the publisher's limit is enforced there.
        if self.band_conditions_fetch.is_none() && now_t >= self.band_conditions_due {
            self.band_conditions_due = now_t + 3600.0;
            self.band_conditions_fetch = persist::spawn_band_conditions_fetch();
        }
        // While the 3D view is open its own feed refreshes the same document,
        // and its copy is at least as fresh as this one.
        #[cfg(not(target_arch = "wasm32"))]
        if self.solar.open
            && let Some(c) = self.solar.band_conditions()
        {
            self.band_conditions = Some(c);
        }

        if now_t - self.daylight_at < 60.0 {
            return;
        }
        self.daylight_at = now_t;
        // No locator, no day or night: the published table is stated for both
        // and picking one at random would be a coin toss shown as a fact. The
        // daytime column is the fallback only because something has to be
        // chosen for the case where nothing is drawn from it anyway.
        self.daylight = sdroxide_types::grid_to_latlon(&self.my_grid()).is_none_or(|(lat, lon)| {
            sdroxide_solar::is_daylight_at(lat, lon, crate::time::now_unix())
        });
    }

    /// The ☀ 3D chip: the solar-system view, in whichever window the platform
    /// gives us.
    ///
    /// Natively that is a second OS window this process owns, so the chip is a
    /// toggle and lights while it is open. In the browser it is a second tab,
    /// which the browser owns — we cannot know whether it is still open and
    /// clicking again should give you another one, so it is a plain button.
    /// The URL is relative so it survives any host, port or reverse proxy.
    ///
    /// `extra` stretches the chip past its label, like the rest of the
    /// condensed Display box's row; every other caller passes 0.
    pub(in crate::app) fn solar_button(&mut self, ui: &mut egui::Ui, extra: f32) {
        let label = super::top_bar::DISPLAY_TOOL_CHIPS[0];
        #[cfg(not(target_arch = "wasm32"))]
        {
            if super::top_bar::chip_stretched(ui, self.solar.open, label, extra)
                .on_hover_text(
                    "Solar system 3D view — Sun, Earth, Moon, sunspots and CMEs (separate window)",
                )
                .clicked()
            {
                self.solar.open = !self.solar.open;
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if super::top_bar::chip_stretched(ui, false, label, extra)
                .on_hover_text(
                    "Solar system 3D view — Sun, Earth, Moon, sunspots and CMEs (new browser tab)",
                )
                .clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab("?view=solar"));
            }
        }
    }
}
