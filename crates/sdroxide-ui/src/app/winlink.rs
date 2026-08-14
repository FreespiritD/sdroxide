//! The MAIL window: Winlink radio email.
//!
//! Modelled on the logbook — an in-root `egui::Window` with a salted id, so a
//! split view can have one per radio — rather than on the 3D map's separate
//! viewport. A mailbox is a searchable record store, which is what the logbook
//! already is.
//!
//! The window holds no mailbox of its own. It asks the engine for a page of a
//! folder and for one message at a time, exactly as the picture gallery does,
//! because on a remote client the mail lives on the machine with the radio and
//! may have attachments in it.

use eframe::egui;
use sdroxide_types::{
    Command, MailDraft, MailEntry, MailFolder, MailListing, MailMessage, WinlinkStatus,
};

/// How many rows to ask for at a time.
const PAGE: u32 = 100;

/// Everything the window remembers between frames.
#[derive(Default)]
pub struct MailUi {
    pub open: bool,
    folder: MailFolder,
    listing: Option<MailListing>,
    /// The message being read, if any.
    selected: Option<Box<MailMessage>>,
    /// Set while a fetch is outstanding, so the pane can say so rather than
    /// showing the previous message as though it were the one clicked.
    loading: Option<String>,
    status: WinlinkStatus,
    /// The compose pane, when open.
    draft: Option<MailDraft>,
    compose_to: String,
    compose_cc: String,
    /// Show the session transcript rather than the message list.
    show_log: bool,
    /// Set when the folder needs re-listing — on open, after a session, after
    /// a delete. Cleared by the request that goes out.
    dirty: bool,
}

impl MailUi {
    pub fn on_status(&mut self, status: WinlinkStatus) {
        // A session that has just finished may have filed new mail, so the
        // open folder is stale.
        if self.status.busy && !status.busy {
            self.dirty = true;
        }
        self.status = status;
    }

    pub fn on_listing(&mut self, listing: MailListing) {
        if listing.folder == self.folder {
            self.listing = Some(listing);
        }
    }

    pub fn on_message(&mut self, msg: Box<MailMessage>) {
        if self.loading.as_deref() == Some(msg.mid.as_str()) {
            self.loading = None;
        }
        self.selected = Some(msg);
    }

    pub fn on_saved(&mut self, _mid: String) {
        self.draft = None;
        self.compose_to.clear();
        self.compose_cc.clear();
        self.dirty = true;
    }

    pub fn on_deleted(&mut self, folder: MailFolder, mid: &str) {
        if self.selected.as_ref().is_some_and(|m| m.mid == mid) {
            self.selected = None;
        }
        if folder == self.folder {
            self.dirty = true;
        }
    }

    fn select_folder(&mut self, folder: MailFolder) {
        if self.folder != folder {
            self.folder = folder;
            self.listing = None;
            self.selected = None;
            self.loading = None;
            self.dirty = true;
        }
    }
}

impl crate::app::SdroxideApp {
    pub(in crate::app) fn mail_window(&mut self, ctx: &egui::Context, cmds: &mut Vec<Command>) {
        if !self.mail.open {
            return;
        }

        // Ask for the folder we are showing, once per change rather than per
        // frame — a listing request walks the mailbox on the engine host.
        if self.mail.dirty {
            self.mail.dirty = false;
            cmds.push(Command::MailList { folder: self.mail.folder, offset: 0, count: PAGE });
        }

        // An `egui::Window` shrinks to its content, so `default_height` alone
        // leaves an empty inbox drawn as a couple of lines. `min_height` keeps
        // a floor, and `set_min_height` inside makes the content actually ask
        // for the room — without that the window collapses again the moment a
        // folder is empty.
        let min_h = crate::layout::window_h(ctx, 520.0);
        let mut open = self.mail.open;
        egui::Window::new("MAIL")
            .id(crate::layout::salted_id(ctx, "MAIL"))
            .open(&mut open)
            .frame(crate::chrome::window_frame())
            .resizable(true)
            .default_width(crate::layout::window_w(ctx, 860.0))
            .default_height(crate::layout::window_h(ctx, 640.0))
            .min_width(crate::layout::window_w(ctx, 420.0))
            .min_height(min_h)
            .show(ctx, |ui| {
                crate::chrome::window_body_bg(ui);
                ui.set_min_height(min_h);
                self.mail_body(ui, cmds);
            });
        self.mail.open = open;
    }

    fn mail_body(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        self.mail_header(ui, cmds);
        ui.separator();

        // Whatever is left under the header. In an auto-sizing window this can
        // come back as zero or infinite, so it is clamped rather than trusted.
        let avail = ui.available_height();
        let body_h = if avail.is_finite() && avail > 1.0 { avail } else { 420.0 };

        if self.mail.show_log {
            self.mail_log(ui, body_h);
            return;
        }
        if self.mail.draft.is_some() {
            self.mail_compose(ui, cmds);
            return;
        }

        // List above, message below — one column, so the window stays usable
        // at the width a browser client on a laptop actually gets. The list
        // keeps a floor so a long message cannot squeeze it away entirely.
        let list_h = (body_h * 0.45).clamp(140.0, (body_h - 120.0).max(140.0));
        egui::ScrollArea::vertical()
            .id_salt("mail-list")
            .min_scrolled_height(list_h)
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.mail_list(ui, cmds);
            });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("mail-body").auto_shrink([false, false]).show(
            ui,
            |ui| {
                self.mail_message(ui, cmds);
            },
        );
    }

    fn mail_header(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        ui.horizontal_wrapped(|ui| {
            for folder in MailFolder::ALL {
                let count = self.mail.status.counts[folder as usize];
                let label = if count > 0 {
                    format!("{} {count}", folder.label())
                } else {
                    folder.label().to_string()
                };
                if ui.selectable_label(self.mail.folder == folder, label).clicked() {
                    self.mail.select_folder(folder);
                }
            }

            ui.separator();

            let busy = self.mail.status.busy;
            if ui.add_enabled(!busy, egui::Button::new("CONNECT")).clicked() {
                cmds.push(Command::WinlinkConnect);
            }
            if ui.add_enabled(!busy, egui::Button::new("COMPOSE")).clicked() {
                self.mail.draft = Some(MailDraft::default());
            }
            if ui.selectable_label(self.mail.show_log, "LOG").clicked() {
                self.mail.show_log = !self.mail.show_log;
            }
        });

        // One status line: what it is doing, or what went wrong last time.
        if self.mail.status.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.mail.status.activity.is_empty() {
                    "connecting…"
                } else {
                    &self.mail.status.activity
                });
            });
        } else if let Some(err) = self.mail.status.last_error.clone() {
            // Errors persist until the next session rather than flashing past:
            // a forwarding failure is the thing the operator most needs to
            // read, and it is often the only clue to why no mail arrived.
            ui.colored_label(crate::theme::ALERT(), format!("last session failed — {err}"));
        } else if self.mail.status.last_session.is_some() {
            ui.label(format!(
                "last session: {} received, {} sent",
                self.mail.status.last_received, self.mail.status.last_sent
            ));
        } else {
            ui.label("not connected yet");
        }
    }

    fn mail_list(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(listing) = self.mail.listing.clone() else {
            ui.label("loading…");
            return;
        };
        if listing.entries.is_empty() {
            ui.label(format!("{} is empty", self.mail.folder.label().to_lowercase()));
            return;
        }

        egui::Grid::new("mail-rows").num_columns(4).striped(true).show(ui, |ui| {
            for entry in &listing.entries {
                self.mail_row(ui, entry, cmds);
                ui.end_row();
            }
        });

        if listing.total as usize > listing.entries.len() {
            ui.label(format!(
                "showing {} of {} — open a message or narrow the folder",
                listing.entries.len(),
                listing.total
            ));
        }
    }

    fn mail_row(&mut self, ui: &mut egui::Ui, entry: &MailEntry, cmds: &mut Vec<Command>) {
        let selected = self.mail.selected.as_ref().is_some_and(|m| m.mid == entry.mid);
        ui.label(&entry.date);
        // Inbox rows show who it is from; everywhere else, who it is to.
        let who = if self.mail.folder == MailFolder::Inbox { &entry.from } else { &entry.to };
        ui.label(who);

        let mut subject = entry.subject.clone();
        if subject.is_empty() {
            subject = "(no subject)".into();
        }
        if entry.attachments > 0 {
            subject.push_str(&format!("  [{}]", entry.attachments));
        }
        if ui.selectable_label(selected, subject).clicked() && !selected {
            self.mail.loading = Some(entry.mid.clone());
            self.mail.selected = None;
            cmds.push(Command::MailGet { folder: self.mail.folder, mid: entry.mid.clone() });
        }

        ui.horizontal(|ui| {
            if self.mail.folder != MailFolder::Archive
                && ui.small_button("archive").on_hover_text("Move to the archive").clicked()
            {
                cmds.push(Command::MailMove {
                    from: self.mail.folder,
                    to: MailFolder::Archive,
                    mid: entry.mid.clone(),
                });
            }
            if ui.small_button("delete").clicked() {
                cmds.push(Command::MailDelete { folder: self.mail.folder, mid: entry.mid.clone() });
            }
        });
    }

    fn mail_message(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        if let Some(mid) = self.mail.loading.clone() {
            ui.label(format!("fetching {mid}…"));
            return;
        }
        let Some(msg) = self.mail.selected.clone() else {
            ui.label("select a message");
            return;
        };

        ui.heading(if msg.subject.is_empty() { "(no subject)" } else { &msg.subject });
        ui.label(format!("from {}   {}", msg.from, msg.date));
        ui.label(format!("to {}", msg.to.join(", ")));
        if !msg.cc.is_empty() {
            ui.label(format!("cc {}", msg.cc.join(", ")));
        }

        if !msg.attachments.is_empty() {
            ui.separator();
            for att in &msg.attachments {
                // Attachments are shown, not saved: the browser client cannot
                // write to the operator's disk, and the native one would be
                // writing somewhere it has not asked about. Naming them is
                // enough to know the message arrived whole.
                ui.label(format!("attachment: {} ({} bytes)", att.name, att.data.len()));
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("REPLY").clicked() {
                let mut subject = msg.subject.clone();
                if !subject.to_ascii_uppercase().starts_with("RE:") {
                    subject = format!("RE: {subject}");
                }
                self.mail.compose_to = msg.from.clone();
                self.mail.compose_cc.clear();
                self.mail.draft = Some(MailDraft {
                    to: vec![msg.from.clone()],
                    cc: vec![],
                    subject,
                    // Quote the original, so a reply over a slow link still
                    // carries the context the recipient needs.
                    body: msg.body.lines().map(|l| format!("> {l}\r\n")).collect(),
                    attachments: vec![],
                });
            }
            if ui.button("DELETE").clicked() {
                cmds.push(Command::MailDelete { folder: msg.folder, mid: msg.mid.clone() });
            }
        });

        ui.separator();
        ui.add(
            egui::TextEdit::multiline(&mut msg.body.as_str())
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
    }

    fn mail_compose(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
        let Some(mut draft) = self.mail.draft.clone() else { return };

        egui::Grid::new("mail-compose").num_columns(2).show(ui, |ui| {
            ui.label("To");
            ui.text_edit_singleline(&mut self.mail.compose_to);
            ui.end_row();
            ui.label("Cc");
            ui.text_edit_singleline(&mut self.mail.compose_cc);
            ui.end_row();
            ui.label("Subject");
            ui.text_edit_singleline(&mut draft.subject);
            ui.end_row();
        });
        ui.label(
            "A callsign, or SMTP:someone@example.org for internet mail. \
             Separate several with commas.",
        );

        ui.separator();
        ui.add(
            egui::TextEdit::multiline(&mut draft.body)
                .desired_width(f32::INFINITY)
                .desired_rows(12)
                .font(egui::TextStyle::Monospace),
        );

        ui.separator();
        ui.horizontal(|ui| {
            let to = split_addresses(&self.mail.compose_to);
            if ui.add_enabled(!to.is_empty(), egui::Button::new("FILE IN OUTBOX")).clicked() {
                draft.to = to;
                draft.cc = split_addresses(&self.mail.compose_cc);
                cmds.push(Command::MailCompose(Box::new(draft.clone())));
            }
            if ui.button("DISCARD").clicked() {
                self.mail.draft = None;
                self.mail.compose_to.clear();
                self.mail.compose_cc.clear();
                return;
            }
            ui.label("filed messages go out on the next session");
        });

        if self.mail.draft.is_some() {
            self.mail.draft = Some(draft);
        }
    }

    fn mail_log(&mut self, ui: &mut egui::Ui, height: f32) {
        if self.mail.status.log.is_empty() {
            ui.label("no session yet");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("mail-log")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.mail.status.log {
                    ui.label(egui::RichText::new(line).monospace());
                }
            });
    }
}

/// Split a comma- or semicolon-separated address list, dropping blanks.
fn split_addresses(text: &str) -> Vec<String> {
    text.split([',', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_address_lists() {
        assert_eq!(split_addresses("OE1XYZ"), ["OE1XYZ"]);
        assert_eq!(split_addresses(" OE1XYZ , OE3ABC ;"), ["OE1XYZ", "OE3ABC"]);
        assert_eq!(split_addresses("SMTP:a@b.org"), ["SMTP:a@b.org"]);
        assert!(split_addresses("  ,  ; ").is_empty());
    }

    #[test]
    fn changing_folder_clears_the_previous_view() {
        let mut ui = MailUi { folder: MailFolder::Inbox, ..Default::default() };
        ui.listing = Some(MailListing { folder: MailFolder::Inbox, ..Default::default() });
        ui.selected = Some(Box::new(MailMessage::default()));

        ui.select_folder(MailFolder::Sent);
        // Stale rows from the old folder must not linger while the new listing
        // is in flight.
        assert!(ui.listing.is_none());
        assert!(ui.selected.is_none());
        assert!(ui.dirty);
    }

    #[test]
    fn a_listing_for_another_folder_is_ignored() {
        // Two requests can be outstanding after fast tab clicks; the late one
        // must not repaint the folder the operator is now looking at.
        let mut ui = MailUi { folder: MailFolder::Sent, ..Default::default() };
        ui.on_listing(MailListing { folder: MailFolder::Inbox, total: 9, ..Default::default() });
        assert!(ui.listing.is_none());
        ui.on_listing(MailListing { folder: MailFolder::Sent, total: 3, ..Default::default() });
        assert_eq!(ui.listing.unwrap().total, 3);
    }

    #[test]
    fn a_finished_session_marks_the_folder_stale() {
        let mut ui = MailUi::default();
        ui.on_status(WinlinkStatus { busy: true, ..Default::default() });
        ui.dirty = false;
        ui.on_status(WinlinkStatus { busy: false, ..Default::default() });
        assert!(ui.dirty, "new mail may have arrived, so the listing must refresh");
    }

    #[test]
    fn deleting_the_open_message_closes_it() {
        let mut ui = MailUi {
            selected: Some(Box::new(MailMessage { mid: "ABC".into(), ..Default::default() })),
            ..Default::default()
        };
        ui.on_deleted(MailFolder::Inbox, "ABC");
        assert!(ui.selected.is_none());
    }
}
