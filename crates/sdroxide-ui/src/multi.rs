//! Multi-radio shell: one window, one radio per tab.
//!
//! Each tab owns a complete [`SdroxideApp`] — controller, view state, panels,
//! decoders' UI — so radios are isolated by construction rather than by a
//! field-by-field split of the app struct. The shell draws the tab strip,
//! delegates the rest of the window to the focused tab, and gives every hidden
//! tab a chance to drain its engine's events each frame: a background radio
//! keeps decoding, logging, recording and reconnecting; it just isn't drawn.
//!
//! What keeps the tabs from stepping on each other lives mostly *below* the
//! UI: each engine has its own config scope (`sdroxide_config::Store`), the
//! [`sdroxide_types::RadioController::set_muted`] path silences one radio's
//! speaker without stopping it, a shared `TxGate` keys one transmitter at a
//! time, and a shared `StoreSync` keeps the station-wide stores (memories,
//! band stacks, digi config) converged across engines. Up here the shell only
//! has to salt the persisted view state per tab and gate the announcer and the
//! window title to the focused one.

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

struct Tab {
    id: u32,
    name: String,
    app: SdroxideApp,
    muted: bool,
}

pub struct MultiApp {
    tabs: Vec<Tab>,
    focused: usize,
    factory: Option<RadioFactory>,
    /// Kept for tabs created at runtime, which are built long after the
    /// [`eframe::CreationContext`] is gone.
    wgpu: Option<eframe::egui_wgpu::RenderState>,
}

impl MultiApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        radios: Vec<RadioTab>,
        factory: Option<RadioFactory>,
    ) -> Self {
        let shared_log = radios.len() > 1;
        let tabs: Vec<Tab> = radios
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
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
                Tab { id: r.id, name: r.name, app, muted: false }
            })
            .collect();
        assert!(!tabs.is_empty(), "MultiApp needs at least one radio");
        MultiApp { tabs, focused: 0, factory, wgpu: cc.wgpu_render_state.clone() }
    }

    /// The main window's strip is a switcher, so it is only drawn once there
    /// is something to switch between — with one radio the window looks
    /// exactly as it always has, and radios are managed from Settings → Radio
    /// (which is also where the second one gets added).
    fn strip_wanted(&self) -> bool {
        self.tabs.len() > 1
    }

    /// The roster as published to the focused tab, for the settings dialog's
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

    /// Act on the radio-management requests the focused tab's settings dialog
    /// queued this frame.
    fn handle_requests(&mut self, reqs: Vec<RadioTabRequest>, ctx: &egui::Context) {
        for req in reqs {
            match req {
                RadioTabRequest::Focus(id) => {
                    if let Some(i) = self.tabs.iter().position(|t| t.id == id)
                        && i != self.focused
                    {
                        // The dialog follows the switch: the operator asked to
                        // work on that radio's settings, not to leave a stale
                        // dialog behind on this one.
                        self.tabs[self.focused].app.close_settings();
                        self.focus_tab(i, ctx);
                        self.tabs[i].app.open_radio_settings();
                    }
                }
                RadioTabRequest::Add => self.add_tab(ctx),
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
                        if let Err(e) = sdroxide_config::rename_radio(id, &name) {
                            eprintln!("sdroxide: renaming radio {id}: {e}");
                        }
                    }
                }
            }
        }
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut focus: Option<usize> = None;
        let mut close: Option<usize> = None;
        let mut add = false;

        egui::Panel::top(egui::Id::new("radio-tab-strip"))
            .frame(
                egui::Frame::new()
                    .fill(crate::theme::BG_DEEP())
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.iter_mut().enumerate() {
                        let selected = i == self.focused;
                        let mut label = RichText::new(Self::display_name(tab)).size(12.5);
                        if selected {
                            label = label.strong().color(crate::theme::TEXT_STRONG());
                        }
                        if crate::chrome::chip(ui, selected, label).clicked() {
                            focus = Some(i);
                        }
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
                            tab.muted = !tab.muted;
                            tab.app.mute_tab(tab.muted);
                        }
                        // The first radio is the station: it runs the shared
                        // network services and the legacy configuration, and
                        // it stays.
                        if i > 0 {
                            let x = crate::chrome::chip(ui, false, RichText::new("×").size(11.0));
                            if x.on_hover_text("Close this radio (its configuration is kept)")
                                .clicked()
                            {
                                close = Some(i);
                            }
                        }
                        ui.add_space(6.0);
                    }
                    if self.factory.is_some()
                        && crate::chrome::chip(ui, false, RichText::new("+").size(13.0))
                            .on_hover_text("Add a radio")
                            .clicked()
                    {
                        add = true;
                    }
                });
            });

        if let Some(i) = focus {
            self.focus_tab(i, ctx);
        }
        if add {
            self.add_tab(ctx);
        }
        if let Some(i) = close {
            self.close_tab(i, ctx);
        }
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
                self.tabs.push(Tab { id: r.id, name: r.name, app, muted: false });
                for tab in &mut self.tabs {
                    tab.app.set_shared_log(true);
                }
                // The dialog follows: if the add came from inside Settings →
                // Radio, that dialog belongs on the new radio now (a no-op
                // when it came from the main window's strip).
                self.tabs[self.focused].app.close_settings();
                let i = self.tabs.len() - 1;
                self.focus_tab(i, ctx);
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

    fn close_tab(&mut self, i: usize, ctx: &egui::Context) {
        if i == 0 || i >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(i);
        tab.app.shutdown_ctrl();
        if let Err(e) = sdroxide_config::remove_radio(tab.id) {
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
    }
}

impl eframe::App for MultiApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        // Hidden tabs first: their engines' unbounded event channels must not
        // back up, and their digital modes keep working in the background.
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i != self.focused {
                tab.app.drain_events(&ctx, now);
            }
        }
        if self.strip_wanted() {
            self.tab_strip(ui, &ctx);
        }
        let f = self.focused.min(self.tabs.len() - 1);
        // Publish the roster before the frame (the settings dialog draws it),
        // act on what the dialog asked for after.
        let roster = self.roster();
        self.tabs[f].app.set_radio_roster(roster);
        eframe::App::ui(&mut self.tabs[f].app, ui, frame);
        let reqs = self.tabs[f].app.take_radio_tab_requests();
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
