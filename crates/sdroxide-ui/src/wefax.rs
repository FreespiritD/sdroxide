//! Client-side state for the weather-fax panel: the chart being painted, and
//! the gallery of ones already saved.
//!
//! The picture is built here as well as in the engine. That is not duplication
//! for its own sake — a remote client sees only the line events, never the
//! engine's buffer, and the panel has to be able to paint a chart that is still
//! twelve minutes from finishing.

use eframe::egui;

/// Rows the live chart grows by. A chart is 1809 pixels wide, so this is about
/// half a megabyte at a time rather than a reallocation per scan line.
const GROW_ROWS: usize = 256;

/// Widest a chart may get before the panel refuses it, as a guard on a length
/// that arrives from the wire.
const MAX_W: usize = 4096;
/// Tallest, matching the demodulator's own limit with headroom.
const MAX_H: usize = 4096;

/// A saved chart in the gallery.
pub struct Chart {
    pub texture: egui::TextureHandle,
    /// File name it was saved under, which carries its timestamp.
    pub name: String,
    pub size: (u16, u16),
}

#[derive(Default)]
pub struct WefaxUi {
    /// Engine status, as of the last update.
    pub status: sdroxide_types::WefaxStatus,
    /// The chart being received: one byte per pixel, top row first.
    live: Vec<u8>,
    live_w: u16,
    live_h: u16,
    /// Which picture `live` belongs to, so a restarted transmission clears it
    /// rather than appending to the previous chart.
    image_id: u32,
    /// The live chart as a texture, rebuilt when rows have arrived.
    live_tex: Option<egui::TextureHandle>,
    dirty: bool,
    /// Saved charts, newest first.
    pub gallery: Vec<Chart>,
    pub loaded_disk: bool,
    /// Which gallery entry is open full-size, if any.
    pub viewing: Option<usize>,
}

impl WefaxUi {
    /// Adopt a freshly decoded scan line.
    pub fn push_line(&mut self, image_id: u32, y: u16, gray: &[u8]) {
        if gray.is_empty() || gray.len() > MAX_W {
            return;
        }
        // A new id, or a row before the write head, means a new transmission.
        if image_id != self.image_id || y == 0 {
            self.image_id = image_id;
            self.live.clear();
            self.live_w = gray.len() as u16;
            self.live_h = 0;
        }
        if gray.len() != self.live_w as usize || self.live_h as usize >= MAX_H {
            return;
        }
        // Rows arrive in order. A gap means lines were dropped somewhere
        // upstream; fill it with mid-grey rather than sliding the rest of the
        // chart up, which would put a seam through the picture instead of a
        // band and be far harder to see.
        while (self.live_h as usize) < y as usize {
            self.live.extend(std::iter::repeat_n(128u8, self.live_w as usize));
            self.live_h += 1;
        }
        if y as usize != self.live_h as usize {
            return;
        }
        if self.live.capacity() < self.live.len() + self.live_w as usize {
            self.live.reserve(self.live_w as usize * GROW_ROWS);
        }
        self.live.extend_from_slice(gray);
        self.live_h += 1;
        self.dirty = true;
    }

    /// Rows received for the chart in progress.
    pub fn live_size(&self) -> (u16, u16) {
        (self.live_w, self.live_h)
    }

    pub fn has_live(&self) -> bool {
        self.live_h > 0
    }

    /// Throw the live chart away — the operator restarting, or a completed one
    /// having moved to the gallery.
    pub fn clear_live(&mut self) {
        self.live.clear();
        self.live_h = 0;
        self.live_tex = None;
        self.dirty = false;
    }

    /// The live chart as a texture, rebuilt only when rows have arrived.
    ///
    /// A full chart is two megapixels and re-uploading it every frame at 120
    /// lines a minute would spend the whole GPU budget on a picture that
    /// changes twice a second.
    pub fn live_texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if self.live_h == 0 {
            return None;
        }
        if self.dirty || self.live_tex.is_none() {
            let img = gray_image(&self.live, self.live_w, self.live_h);
            match &mut self.live_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.live_tex =
                        Some(ctx.load_texture("wefax-live", img, egui::TextureOptions::LINEAR))
                }
            }
            self.dirty = false;
        }
        self.live_tex.as_ref()
    }

    /// Add a decoded PNG to the front of the gallery.
    pub fn add_chart(&mut self, ctx: &egui::Context, name: &str, png: &[u8]) {
        let Some((gray, w, h)) = decode_gray(png) else { return };
        let texture = ctx.load_texture(
            format!("wefax-{name}"),
            gray_image(&gray, w, h),
            egui::TextureOptions::LINEAR,
        );
        self.gallery.insert(0, Chart { texture, name: name.to_string(), size: (w, h) });
    }
}

/// Decode a PNG to a single-channel raster plus its size.
pub fn decode_gray(png: &[u8]) -> Option<(Vec<u8>, u16, u16)> {
    let img = image::load_from_memory(png).ok()?.to_luma8();
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 || w as usize > MAX_W * 4 || h as usize > MAX_H * 4 {
        return None;
    }
    Some((img.into_raw(), w as u16, h as u16))
}

/// A single-channel raster as an egui image.
pub fn gray_image(gray: &[u8], w: u16, h: u16) -> egui::ColorImage {
    let (w, h) = (w as usize, h as usize);
    let mut px = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let v = gray.get(i).copied().unwrap_or(0);
        px.push(egui::Color32::from_gray(v));
    }
    egui::ColorImage { size: [w, h], pixels: px, source_size: egui::vec2(w as f32, h as f32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_build_the_chart_downwards() {
        let mut ui = WefaxUi::default();
        for y in 0..4u16 {
            ui.push_line(1, y, &[y as u8; 8]);
        }
        assert_eq!(ui.live_size(), (8, 4));
        assert_eq!(ui.live[0], 0);
        assert_eq!(ui.live[8], 1);
        assert_eq!(ui.live[24], 3);
        assert!(ui.has_live());
    }

    /// A gap in the rows has to leave a band, not slide the rest of the chart
    /// up — a seam through the middle of a weather map is much harder to spot
    /// than a grey stripe, and it puts every isobar below it in the wrong place.
    #[test]
    fn a_dropped_row_leaves_a_band_rather_than_a_seam() {
        let mut ui = WefaxUi::default();
        ui.push_line(1, 0, &[10; 4]);
        ui.push_line(1, 3, &[20; 4]);
        assert_eq!(ui.live_size(), (4, 4));
        assert_eq!(ui.live[0], 10);
        assert_eq!(&ui.live[4..12], &[128; 8], "the gap should be mid-grey");
        assert_eq!(ui.live[12], 20);
    }

    /// A new transmission starts a new picture rather than appending to the
    /// last one.
    #[test]
    fn a_new_transmission_starts_a_new_chart() {
        let mut ui = WefaxUi::default();
        ui.push_line(1, 0, &[1; 4]);
        ui.push_line(1, 1, &[2; 4]);
        assert_eq!(ui.live_size(), (4, 2));
        ui.push_line(2, 0, &[3; 6]);
        assert_eq!(ui.live_size(), (6, 1), "the width should follow the new picture");
        assert_eq!(ui.live[0], 3);
        ui.clear_live();
        assert!(!ui.has_live());
        assert_eq!(ui.live_size(), (6, 0));
    }

    /// Rubbish off the wire must be refused rather than sized into an
    /// allocation, and a row that does not match the picture's width dropped.
    #[test]
    fn malformed_rows_are_refused() {
        let mut ui = WefaxUi::default();
        ui.push_line(1, 0, &[]);
        assert!(!ui.has_live());
        ui.push_line(1, 0, &vec![0u8; MAX_W + 1]);
        assert!(!ui.has_live());
        // A width change mid-picture is not a picture.
        ui.push_line(1, 0, &[1; 8]);
        ui.push_line(1, 1, &[1; 9]);
        assert_eq!(ui.live_size(), (8, 1));
        // A row behind the write head is dropped rather than overwriting.
        ui.push_line(1, 5, &[2; 8]);
        assert_eq!(ui.live_size(), (8, 6), "the gap is filled forwards");
    }

    #[test]
    fn a_raster_becomes_a_grey_image_of_the_right_size() {
        let img = gray_image(&[0, 128, 255, 64], 2, 2);
        assert_eq!(img.size, [2, 2]);
        assert_eq!(img.pixels[0], egui::Color32::from_gray(0));
        assert_eq!(img.pixels[2], egui::Color32::from_gray(255));
        // A short buffer is padded rather than panicking.
        assert_eq!(gray_image(&[1], 2, 2).pixels.len(), 4);
    }
}
