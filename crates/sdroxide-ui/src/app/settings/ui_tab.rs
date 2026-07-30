//! The UI tab: frame rate, spectrum averaging, waterfall speed and palette.
//!
//! These take effect live — the frame rate and averaging reach the engine
//! through the spectrum-config diff on the next frame, and the waterfall speed
//! is read straight out of the settings each frame.

use eframe::egui::{self, ComboBox, RichText};

use crate::colormap;

use crate::app::settings::enum_combo;

pub(in crate::app) fn settings_ui_tab(
    ui: &mut egui::Ui,
    cfg: &mut sdroxide_types::UiSettings,
    cloud_march: Option<&mut bool>,
) {
    use sdroxide_types::{Speed, UiSettings};
    ui.label(RichText::new("Display").size(14.0).strong().color(crate::theme::CYAN));
    ui.add_space(6.0);
    egui::Grid::new("ui-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Screen update rate");
        ComboBox::from_id_salt("ui-fps")
            .selected_text(format!("{} fps", cfg.frame_rate_fps))
            .show_ui(ui, |ui| {
                for f in UiSettings::FPS_OPTIONS {
                    ui.selectable_value(&mut cfg.frame_rate_fps, f, format!("{f} fps"));
                }
            });
        ui.end_row();

        ui.label("Waterfall scroll speed");
        enum_combo(ui, "ui-wf", &mut cfg.waterfall_speed, &Speed::ALL, Speed::label);
        ui.end_row();

        ui.label("Spectrum update speed");
        enum_combo(ui, "ui-spec", &mut cfg.spectrum_speed, &Speed::ALL, Speed::label);
        ui.end_row();

        ui.label("Waterfall palette");
        ComboBox::from_id_salt("ui-palette")
            .selected_text(colormap::NAMES[cfg.waterfall_palette.min(colormap::NAMES.len() - 1)])
            .show_ui(ui, |ui| {
                for (i, name) in colormap::NAMES.iter().enumerate() {
                    ui.selectable_value(&mut cfg.waterfall_palette, i, *name);
                }
            });
        ui.end_row();

        ui.label("Spectrum background");
        ui.horizontal(|ui| {
            ui.checkbox(&mut cfg.spectrum_gradient, "Gradient");
            ui.add_enabled_ui(cfg.spectrum_gradient, |ui| {
                ui.label("top");
                ui.color_edit_button_srgb(&mut cfg.gradient_top);
                ui.label("bottom");
                ui.color_edit_button_srgb(&mut cfg.gradient_bottom);
            });
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "Higher frame rates look smoother but cost more CPU/GPU. Spectrum speed \
             sets how quickly the trace reacts (slower = smoother/more averaged). The \
             background gradient fills the spectrum area from the top colour down to \
             the bottom colour.",
        )
        .weak(),
    );

    let Some(cloud_march) = cloud_march else { return };
    ui.add_space(14.0);
    ui.label(RichText::new("3D view").size(14.0).strong().color(crate::theme::CYAN));
    ui.add_space(6.0);
    egui::Grid::new("ui-grid-3d").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Cloud rendering");
        ComboBox::from_id_salt("ui-cloud-march")
            .selected_text(if *cloud_march { "Volumetric" } else { "Layered" })
            .show_ui(ui, |ui| {
                ui.selectable_value(cloud_march, false, "Layered");
                ui.selectable_value(cloud_march, true, "Volumetric");
            });
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "How the CLOUDS layer in the 3D view draws the weather. Layered stacks \
             slices through the troposphere and is the cheap option. Volumetric walks \
             a ray through it instead, so the Sun casts the cloud tops onto the deck \
             below and lightning glows out through the storm making it rather than \
             only brightening its outside — at several times the cost per pixel.",
        )
        .weak(),
    );
}
