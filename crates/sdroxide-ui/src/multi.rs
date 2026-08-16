//! Multi-radio shell: one window, one radio per tab — or several side by
//! side.
//!
//! Each tab owns a complete [`SdroxideApp`] — controller, view state, panels,
//! decoders' UI — so radios are isolated by construction rather than by a
//! field-by-field split of the app struct. The shell draws the radio strip,
//! hands the rest of the window to the radios on screen, and gives every
//! hidden tab a chance to drain its engine's events each frame: a background
//! radio keeps decoding, logging, recording and reconnecting; it just isn't
//! drawn.
//!
//! The screen itself is a row of *panes* ([`MultiApp::panes`]). One pane is
//! the ordinary window; the strip's ⊞ toggles give further radios a pane of
//! their own, splitting the main area into equal columns that each carry
//! their own copy of the strip — so any pane can be switched to any radio
//! that isn't already on screen. Exactly one visible radio holds the
//! keyboard/MIDI focus; each [`SdroxideApp`] gates its input handling on it,
//! and clicking a pane moves it there.
//!
//! What keeps the radios from stepping on each other lives mostly *below*
//! the UI: each engine has its own config scope (`sdroxide_config::Store`),
//! the [`sdroxide_types::RadioController::set_muted`] path silences one
//! radio's speaker without stopping it, a shared `TxGate` keys one
//! transmitter at a time, and a shared `StoreSync` keeps the station-wide
//! stores (memories, band stacks, digi config) converged across engines. Up
//! here the shell only has to salt the persisted view state per tab (and,
//! through `layout::set_radio_salt`, the fixed egui ids) and gate the
//! announcer and the window title to the focused one.

use eframe::egui::{self, RichText};

use crate::app::{RadioChip, RadioTabRequest, SdroxideApp};
use sdroxide_types::RadioController;

/// One radio handed to [`MultiApp::new`] by the frontend.
pub struct RadioTab {
    pub id: u32,
    pub name: String,
    pub ctrl: Box<dyn RadioController>,
}

/// Builds the engine + controller for a radio created at runtime from the
/// "+" chip. Lives in the binary — only it knows how to open backends.
pub type RadioFactory = Box<dyn FnMut() -> Result<RadioTab, String>>;

/// Dials another station for the Remote settings tab: `(url, id, ctx)` in,
/// a connected tab out. Also in the binary, for the same reason — the
/// connection needs this machine's sound devices hung off it, and only the
/// frontend can open those. The context is what the socket wakes when a
/// message arrives.
pub type RemoteFactory = Box<dyn FnMut(&str, u32, &egui::Context) -> Result<RadioTab, String>>;

/// Ids for tabs that hold somebody else's station. The station's own radios
/// are numbered from the roster on disk, from 0 up; these are not in it — a
/// connection is not a radio this machine owns — so they are numbered from a
/// place the roster will never reach rather than allocated from it. The id
/// keys the tab's view state and its waterfall history, and a collision would
/// hand a connection the layout and the history of one of this machine's own
/// radios.
const REMOTE_TAB_ID_BASE: u32 = 1 << 31;

struct Tab {
    id: u32,
    name: String,
    app: SdroxideApp,
    muted: bool,
    /// Somebody else's station, reached over the network. It has no entry in
    /// this machine's radio roster, so closing or renaming it must not go
    /// looking for one.
    remote: bool,
}

/// What the operator asked a strip to do, resolved after the frame — the
/// strips draw while the tabs are borrowed for display, so clicks are
/// collected as values first (the same arrangement as [`RadioTabRequest`]).
enum StripAction {
    /// A name-chip click: show this radio on this pane.
    Show {
        pane: usize,
        id: u32,
    },
    /// The ⊞ toggle: give this radio a pane of its own, or close the one it
    /// has.
    ToggleSplit(u32),
    Mute {
        id: u32,
        muted: bool,
    },
    Add,
}

pub struct MultiApp {
    tabs: Vec<Tab>,
    focused: usize,
    /// The radios on screen, left to right, by tab id. One entry is the
    /// ordinary single-radio window; more than one splits the main area into
    /// that many equal columns. Kept duplicate-free — the same radio twice
    /// would be two views fighting over one engine's spectrum stream — never
    /// empty, and always holding the focused tab ([`Self::sanitize_panes`]).
    panes: Vec<u32>,
    factory: Option<RadioFactory>,
    /// How a station somewhere else is dialled — Settings → Remote. Present
    /// in every native session, including one that is itself a remote client:
    /// a screen with no radio of its own is exactly the one most likely to be
    /// pointed at a server.
    remote: Option<RemoteFactory>,
    /// Kept for tabs created at runtime, which are built long after the
    /// [`eframe::CreationContext`] is gone.
    wgpu: Option<eframe::egui_wgpu::RenderState>,
}

impl MultiApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        radios: Vec<RadioTab>,
        factory: Option<RadioFactory>,
        remote: Option<RemoteFactory>,
    ) -> Self {
        let shared_log = radios.len() > 1;
        let can_add = factory.is_some();
        let tabs: Vec<Tab> = radios
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let remote = r.ctrl.engine_is_remote();
                let mut app = SdroxideApp::new_tab(
                    &cc.egui_ctx,
                    cc.storage,
                    cc.wgpu_render_state.clone(),
                    r.ctrl,
                    r.id,
                    i == 0,
                );
                app.set_focused_flag(i == 0);
                app.set_shared_log(shared_log);
                app.set_can_add_radio(can_add);
                Tab { id: r.id, name: r.name, app, muted: false, remote }
            })
            .collect();
        assert!(!tabs.is_empty(), "MultiApp needs at least one radio");
        let panes = vec![tabs[0].id];
        MultiApp { tabs, focused: 0, panes, factory, remote, wgpu: cc.wgpu_render_state.clone() }
    }

    /// The main window's strip is a switcher, so it is only drawn once there
    /// is something to switch between — with one radio the window looks
    /// exactly as it always has, and radios are managed from Settings → Radio
    /// (which is also where the second one gets added, and the only place a
    /// radio can be deleted from).
    fn strip_wanted(&self) -> bool {
        self.tabs.len() > 1
    }

    /// The roster as published to the visible tabs, for the settings dialog's
    /// copy of the strip.
    fn roster(&self) -> Vec<RadioChip> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| RadioChip {
                id: t.id,
                name: t.name.clone(),
                default_name: Self::default_name(t),
                tx_on: t.app.tab_tx_on(),
                error: t.app.tab_error(),
                muted: t.muted,
                focused: i == self.focused,
            })
            .collect()
    }

    /// What an unrenamed tab is called: the interface its radio runs, or a
    /// neutral "Radio N" while it has none to be named after. The number is
    /// the id the engine's own messages use ("radio 2 is on the air"), so the
    /// two always agree.
    fn default_name(t: &Tab) -> String {
        t.app.interface_name().unwrap_or_else(|| format!("Radio {}", t.id + 1))
    }

    /// The name a tab chip shows: the operator's, else the derived default.
    fn display_name(t: &Tab) -> String {
        if t.name.is_empty() { Self::default_name(t) } else { t.name.clone() }
    }

    /// Re-establish the pane invariants after anything changed the roster or
    /// the panes: every pane shows an existing radio, no radio twice, at
    /// least one pane, and the focused tab is on screen.
    fn sanitize_panes(&mut self, ctx: &egui::Context) {
        self.focused = self.focused.min(self.tabs.len() - 1);
        let mut seen: Vec<u32> = Vec::new();
        self.panes.retain(|&id| {
            let keep = !seen.contains(&id) && self.tabs.iter().any(|t| t.id == id);
            seen.push(id);
            keep
        });
        if self.panes.is_empty() {
            self.panes.push(self.tabs[self.focused].id);
        }
        if !self.panes.contains(&self.tabs[self.focused].id) {
            let first = self.panes[0];
            if let Some(i) = self.tabs.iter().position(|t| t.id == first) {
                self.focus_tab(i, ctx);
            }
        }
    }

    /// The pane the focused radio sits on — where "switch to radio X"
    /// requests land while the view is split.
    fn active_pane(&self) -> usize {
        let fid = self.tabs[self.focused].id;
        self.panes.iter().position(|&p| p == fid).unwrap_or(0)
    }

    /// Put radio `id` on pane `pane` and hand it the keyboard. A radio that
    /// is already on screen only takes focus — the strip disables its name
    /// chip everywhere else, so the same radio is never shown twice.
    fn show_in_pane(&mut self, pane: usize, id: u32, ctx: &egui::Context) {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return };
        if !self.panes.contains(&id) && pane < self.panes.len() {
            self.panes[pane] = id;
        }
        self.focus_tab(i, ctx);
    }

    /// The ⊞ toggle: open a pane for radio `id`, or close the one it has.
    fn toggle_split(&mut self, id: u32, ctx: &egui::Context) {
        if let Some(k) = self.panes.iter().position(|&p| p == id) {
            // The last pane is not a split; it stays.
            if self.panes.len() > 1 {
                self.panes.remove(k);
                // The keyboard must stay on a visible radio: hand it to the
                // pane that slid into the closed one's place.
                if !self.panes.contains(&self.tabs[self.focused].id) {
                    let next = self.panes[k.min(self.panes.len() - 1)];
                    if let Some(i) = self.tabs.iter().position(|t| t.id == next) {
                        self.focus_tab(i, ctx);
                    }
                }
            }
        } else if self.tabs.iter().any(|t| t.id == id) {
            self.panes.push(id);
        }
    }

    /// Act on the radio-management requests the visible tabs' settings
    /// dialogs queued this frame.
    fn handle_requests(&mut self, reqs: Vec<RadioTabRequest>, ctx: &egui::Context) {
        for req in reqs {
            match req {
                RadioTabRequest::Focus(id) => {
                    if let Some(i) = self.tabs.iter().position(|t| t.id == id)
                        && i != self.focused
                    {
                        // The dialog follows the switch: the operator asked to
                        // work on that radio's settings, not to leave a stale
                        // dialog behind on this one. In a split the target
                        // takes over the active pane rather than adding one.
                        self.tabs[self.focused].app.close_settings();
                        self.show_in_pane(self.active_pane(), id, ctx);
                        self.tabs[i].app.open_radio_settings();
                    }
                }
                RadioTabRequest::Add => self.add_tab(ctx),
                RadioTabRequest::Connect { url, name } => self.connect_tab(&url, name, ctx),
                RadioTabRequest::Close(id) => {
                    if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                        self.close_tab(i, ctx);
                    }
                }
                RadioTabRequest::Mute { id, muted } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                        t.muted = muted;
                        t.app.mute_tab(muted);
                    }
                }
                RadioTabRequest::Rename { id, name } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                        t.name = name.clone();
                        // A connection has no roster entry to record it in —
                        // the name it was given is the address it was dialled
                        // at, and it lasts as long as the connection does.
                        if !t.remote
                            && let Err(e) = sdroxide_config::rename_radio(id, &name)
                        {
                            eprintln!("sdroxide: renaming radio {id}: {e}");
                        }
                    }
                }
            }
        }
    }

    /// What the strip asked for, applied once the tabs are no longer
    /// borrowed for drawing.
    fn apply_strip_actions(&mut self, actions: Vec<StripAction>, ctx: &egui::Context) {
        for act in actions {
            match act {
                StripAction::Show { pane, id } => self.show_in_pane(pane, id, ctx),
                StripAction::ToggleSplit(id) => self.toggle_split(id, ctx),
                StripAction::Mute { id, muted } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                        t.muted = muted;
                        t.app.mute_tab(muted);
                    }
                }
                StripAction::Add => self.add_tab(ctx),
            }
        }
    }

    /// One strip: every radio's controls in a box of their own, then the "+"
    /// chip. `pane` is the split column the strip sits on (0 in the single
    /// view), so a name click knows which pane it re-assigns. Closing a radio
    /// is deliberately absent — that lives in Settings → Radio, behind a
    /// dialog, not one stray click away on the main window.
    fn strip_row(&self, ui: &mut egui::Ui, pane: usize, actions: &mut Vec<StripAction>) {
        crate::chrome::tab_bar(ui, |ui, bar| {
            for (i, tab) in self.tabs.iter().enumerate() {
                let id = tab.id;
                let here = self.panes.get(pane) == Some(&id);
                let elsewhere = !here && self.panes.contains(&id);
                let split_on = self.panes.len() > 1 && self.panes.contains(&id);
                // In a split the accent marks the pane holding the keyboard.
                let accent = if here && split_on && i == self.focused {
                    crate::theme::PINK()
                } else {
                    crate::theme::CYAN()
                };
                // A real tab, not a chip: this strip decides which radio the
                // page below belongs to, which is the one thing a row of
                // buttons cannot say. The mute and split toggles ride inside
                // their own tab the way a close box rides in a browser's.
                let body = bar.tab_body_accent(ui, here, accent, |ui| {
                    let mut label = RichText::new(Self::display_name(tab)).size(12.5);
                    if here {
                        label = label.strong().color(crate::theme::TEXT_STRONG());
                    } else if elsewhere {
                        label = label.weak();
                    }
                    ui.label(label);
                    // On the air: the one thing worth seeing from any tab.
                    if tab.app.tab_tx_on() {
                        ui.label(RichText::new("● TX").size(11.0).color(crate::theme::ALERT()));
                    } else if tab.app.tab_error() {
                        ui.label(RichText::new("⚠").size(11.0).color(crate::theme::ALERT()));
                    }
                    let mute = crate::chrome::chip(
                        ui,
                        tab.muted,
                        RichText::new(if tab.muted { "🔇" } else { "🔊" }).size(11.0),
                    );
                    if mute.on_hover_text("Mute this radio's audio").clicked() {
                        actions.push(StripAction::Mute { id, muted: !tab.muted });
                    }
                    let split = crate::chrome::chip(ui, split_on, RichText::new("⊞").size(11.0));
                    let tip = if split_on {
                        "Close this radio's split view"
                    } else {
                        "Open this radio in a split view of its own"
                    };
                    if split.on_hover_text(tip).clicked() {
                        actions.push(StripAction::ToggleSplit(id));
                    }
                });
                // The tab itself switches the pane — anywhere on it that is not
                // one of its own buttons, which keep their clicks.
                let body = if elsewhere {
                    body.response.on_hover_text("Already open in another split view")
                } else if here {
                    body.response
                } else {
                    body.response.on_hover_text("Show this radio here")
                };
                if body.clicked() && !here && !elsewhere {
                    actions.push(StripAction::Show { pane, id });
                }
            }
            if self.factory.is_some() {
                bar.end_tabs(ui);
                if crate::chrome::chip(ui, false, RichText::new("+").size(13.0))
                    .on_hover_text("Add a radio")
                    .clicked()
                {
                    actions.push(StripAction::Add);
                }
            }
        });
    }

    fn focus_tab(&mut self, i: usize, ctx: &egui::Context) {
        if i == self.focused || i >= self.tabs.len() {
            return;
        }
        self.tabs[self.focused].app.set_focused(false, ctx);
        self.focused = i;
        self.tabs[i].app.set_focused(true, ctx);
    }

    fn add_tab(&mut self, ctx: &egui::Context) {
        let Some(factory) = self.factory.as_mut() else { return };
        match factory() {
            Ok(r) => {
                let i = self.install_tab(r, ctx);
                // The new tab has no interface yet; the operator's next stop
                // is Settings → Radio, so open it for them.
                self.tabs[i].app.open_radio_settings();
            }
            // Into the focused tab's dismissable banner — the main window's
            // strip may not be on screen to carry a message.
            Err(e) => {
                self.tabs[self.focused].app.show_notice(format!("Could not add a radio: {e}"))
            }
        }
    }

    /// Dial another sdroxide server and give it a tab of its own — Settings →
    /// Remote's CONNECT button.
    ///
    /// The verdict goes back to the tab that asked, not to the new one: a
    /// connection that never opened has no tab to report from, and the dialog
    /// the operator is looking at is where they are expecting an answer.
    fn connect_tab(&mut self, url: &str, name: String, ctx: &egui::Context) {
        let origin = self.focused;
        let Some(remote) = self.remote.as_mut() else {
            self.tabs[origin]
                .app
                .set_remote_status(Err("This client cannot open a connection.".into()));
            return;
        };
        // Never an id from the roster: this station is not one of ours.
        let id = self
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|id| *id >= REMOTE_TAB_ID_BASE)
            .max()
            .map_or(REMOTE_TAB_ID_BASE, |m| m.saturating_add(1));
        match remote(url, id, ctx) {
            Ok(mut r) => {
                // The address as the operator entered it wins over whatever
                // the factory called it — it is what they will recognise in
                // the strip.
                r.name = name;
                let label = r.name.clone();
                self.install_tab(r, ctx);
                // Not "connected": the socket opens in the background, and
                // whether the station answers is reported by the tab it now
                // has — with the sign-in screen, if it asks for one.
                self.tabs[origin]
                    .app
                    .set_remote_status(Ok(format!("{label} has a tab of its own now.")));
            }
            Err(e) => self.tabs[origin].app.set_remote_status(Err(format!("{url}: {e}"))),
        }
    }

    /// Put a freshly built radio on screen: append it, take over the active
    /// pane, hand it the keyboard. Returns its index.
    fn install_tab(&mut self, r: RadioTab, ctx: &egui::Context) -> usize {
        let new_id = r.id;
        let remote = r.ctrl.engine_is_remote();
        let mut app = SdroxideApp::new_tab(
            ctx,
            // A brand-new radio has no saved view to restore, and the
            // station-wide settings are read from their real files on
            // native; storage is only a wasm concern, and wasm has no
            // factory.
            None,
            self.wgpu.clone(),
            r.ctrl,
            r.id,
            false,
        );
        app.set_focused_flag(false);
        app.set_can_add_radio(self.factory.is_some());
        self.tabs.push(Tab { id: r.id, name: r.name, app, muted: false, remote });
        for tab in &mut self.tabs {
            tab.app.set_shared_log(true);
        }
        // The dialog follows: if the request came from inside Settings, that
        // dialog belongs on the new radio now (a no-op when it came from the
        // main window's strip).
        self.tabs[self.focused].app.close_settings();
        // In a split the new radio takes over the active pane rather than
        // opening yet another column unasked.
        let pane = self.active_pane();
        let i = self.tabs.len() - 1;
        self.panes[pane] = new_id;
        self.focus_tab(i, ctx);
        i
    }

    fn close_tab(&mut self, i: usize, ctx: &egui::Context) {
        if i == 0 || i >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(i);
        tab.app.shutdown_ctrl();
        // Closing a connection hangs up and nothing more: it was never in this
        // machine's roster, and the station at the other end is not ours to
        // remove from anything.
        if !tab.remote
            && let Err(e) = sdroxide_config::remove_radio(tab.id)
        {
            eprintln!("sdroxide: removing radio {} from the roster: {e}", tab.id);
        }
        crate::waterfall_gpu::retire(self.wgpu.as_ref(), u64::from(tab.id));
        // The removal shifted everything after `i` down by one.
        let was_focused = self.focused == i;
        if self.focused > i {
            self.focused -= 1;
        }
        if was_focused {
            self.focused = self.focused.min(self.tabs.len() - 1);
            self.tabs[self.focused].app.set_focused(true, ctx);
        }
        if self.tabs.len() == 1 {
            self.tabs[0].app.set_shared_log(false);
        }
        // Its pane goes with it (and the focus moves onto a visible radio).
        self.sanitize_panes(ctx);
    }
}

impl eframe::App for MultiApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.sanitize_panes(&ctx);
        // Pane order → tab index; `sanitize_panes` guaranteed each exists.
        let pane_tabs: Vec<usize> =
            self.panes.iter().filter_map(|id| self.tabs.iter().position(|t| t.id == *id)).collect();
        // Hidden tabs first: their engines' unbounded event channels must not
        // back up, and their digital modes keep working in the background.
        // (The visible ones drain at the top of their own frame loop.)
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if !pane_tabs.contains(&i) {
                tab.app.drain_events(&ctx, now);
            }
        }
        // Publish the roster before the frame (the settings dialog draws it),
        // act on what the strips and the dialogs asked for after it.
        let roster = self.roster();
        let mut actions: Vec<StripAction> = Vec::new();
        let mut reqs: Vec<RadioTabRequest> = Vec::new();

        if pane_tabs.len() == 1 {
            if self.strip_wanted() {
                egui::Panel::top(egui::Id::new("radio-tab-strip"))
                    .frame(
                        // No bottom margin: the tabs' baseline is the top edge
                        // of the page below them, and a gap under it would
                        // leave the strip floating over the page instead.
                        egui::Frame::new()
                            .fill(crate::theme::BG_DEEP())
                            .inner_margin(egui::Margin { left: 8, right: 8, top: 3, bottom: 0 }),
                    )
                    .show(ui, |ui| self.strip_row(ui, 0, &mut actions));
            }
            let f = pane_tabs[0];
            self.tabs[f].app.set_radio_roster(roster);
            eframe::App::ui(&mut self.tabs[f].app, ui, frame);
            reqs.append(&mut self.tabs[f].app.take_radio_tab_requests());
        } else {
            // Split view: one equal column per pane, each under its own strip.
            let avail = ui.available_rect_before_wrap();
            let n = pane_tabs.len() as f32;
            let gap = 6.0;
            let col_w = ((avail.width() - gap * (n - 1.0)) / n).max(50.0);
            for (k, &ti) in pane_tabs.iter().enumerate() {
                let left = avail.left() + k as f32 * (col_w + gap);
                let col = egui::Rect::from_min_max(
                    egui::pos2(left, avail.top()),
                    egui::pos2(left + col_w, avail.bottom()),
                );
                let id = self.tabs[ti].id;
                let mut pane_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(col)
                        .layout(egui::Layout::top_down(egui::Align::Min))
                        .id_salt(("radio-pane", id)),
                );
                pane_ui.shrink_clip_rect(col);
                // This pane's strip — switches what the pane shows.
                egui::Frame::new()
                    .fill(crate::theme::BG_DEEP())
                    .inner_margin(egui::Margin { left: 8, right: 8, top: 3, bottom: 0 })
                    .show(&mut pane_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.strip_row(ui, k, &mut actions);
                    });
                self.tabs[ti].app.set_radio_roster(roster.clone());
                eframe::App::ui(&mut self.tabs[ti].app, &mut pane_ui, frame);
                reqs.append(&mut self.tabs[ti].app.take_radio_tab_requests());
                if k + 1 < pane_tabs.len() {
                    ui.painter().vline(
                        col.right() + gap * 0.5,
                        avail.y_range(),
                        egui::Stroke::new(1.0, crate::theme::PANEL()),
                    );
                }
            }
            ui.allocate_rect(avail, egui::Sense::hover());
            // A press on a pane's background hands it the keyboard. Floating
            // windows live on layers above the background, so working in a
            // dialog that overhangs a neighbouring pane doesn't move focus.
            if ctx.input(|i| i.pointer.any_pressed())
                && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
                && ctx.layer_id_at(pos).is_none_or(|l| l.order == egui::Order::Background)
            {
                for (k, &ti) in pane_tabs.iter().enumerate() {
                    let left = avail.left() + k as f32 * (col_w + gap);
                    if ti != self.focused && pos.x >= left && pos.x < left + col_w + gap {
                        self.focus_tab(ti, &ctx);
                    }
                }
            }
        }
        self.apply_strip_actions(actions, &ctx);
        self.handle_requests(reqs, &ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        for tab in &mut self.tabs {
            eframe::App::save(&mut tab.app, storage);
        }
    }

    fn on_exit(&mut self) {
        // Joining every engine here is what lets each device close before
        // process teardown can race the C libraries' own exit handlers.
        for tab in &mut self.tabs {
            tab.app.shutdown_ctrl();
        }
    }
}
