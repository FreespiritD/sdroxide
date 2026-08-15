//! The sdroxide GUI: an egui app that talks to any [`sdroxide_types::RadioController`].
//!
//! Compiles native and for wasm32. All custom wgpu rendering that the browser
//! build shares is written to WebGL2 downlevel limits (fragment-only, sampled
//! textures + uniforms). The one exception is [`solar3d`], which uses depth,
//! MSAA and vertex buffers — but does so entirely inside an offscreen pass of
//! its own, so the shared egui pass never sees them and the module runs on both
//! targets.

mod app;
pub mod chrome;
mod colormap;
mod digi_map;
mod download;
mod fuzzy;
mod hell;
mod help;
mod input;
/// Which layout the window wears — desktop strip, tablet menus, or the compact
/// phone strip — and the metrics that follow from it.
pub mod layout;
mod login;
mod login_globe;
/// Multi-radio shell: one window, one radio per tab. Native-only — the
/// browser client drives a single (remote) radio.
#[cfg(not(target_arch = "wasm32"))]
mod multi;
mod prop_map;
#[cfg(feature = "remote")]
mod remote;
mod rf_paint;
/// Solar-system 3D view. A second OS window natively, a second browser tab on
/// the web; the sole consumer of depth/MSAA/vertex buffers either way.
mod solar3d;
mod sstv;
pub mod theme;
mod time;
mod view;
mod waterfall_gpu;
mod wefax;
mod widgets;

pub use app::SdroxideApp;
#[cfg(not(target_arch = "wasm32"))]
pub use multi::{MultiApp, RadioFactory, RadioTab, RemoteFactory};
#[cfg(feature = "remote")]
pub use remote::{AudioBridge, RemoteController};
/// The solar-system view as a standalone app, for the browser tab the ☀ 3D
/// chip opens. Natively the same view is a child viewport of the main window
/// instead — see `solar3d::Solar3d`.
#[cfg(feature = "remote")]
pub use solar3d::SolarApp;

/// Wgpu access must go through this re-export so every crate agrees on the
/// wgpu version (project rule).
pub use eframe::egui_wgpu;

/// The wgpu setup every sdroxide window opens with.
///
/// eframe's default asks the device for `wgpu::Limits::default()` — the WebGPU
/// baseline — on every backend but GL. Real GPUs are allowed to sit *below*
/// that baseline, and the request then fails outright rather than degrading,
/// taking the window with it: a Raspberry Pi 5 (V3D) grants 15 inter-stage
/// shader variables where the baseline asks for 16, and eframe exits with
///
/// > Limit 'max_inter_stage_shader_variables' value 16 is better than allowed 15
///
/// Nothing here needs the baseline. The busiest shader in the tree passes three
/// varyings, and every texture is already built against `device.limits()` —
/// `solar3d` and `login_globe` pick the mip level that fits, and the
/// waterfall's atlas is a fixed 2048². So ask for exactly what the adapter
/// reports, a request no adapter can refuse, and let the drawing code adapt to
/// what it gets, which is what it already does.
///
/// This also *lifts* limits on the GL backend, where eframe asks for the WebGL2
/// downlevel defaults: a native GL context that can do better now says so, and
/// the globe gets its full-resolution maps instead of the 2048-pixel cap.
pub fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    use egui_wgpu::wgpu;
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor =
        std::sync::Arc::new(|adapter: &wgpu::Adapter| wgpu::DeviceDescriptor {
            label: Some("sdroxide"),
            required_limits: adapter.limits(),
            ..Default::default()
        });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// The application icon, for [`eframe::egui::ViewportBuilder::with_icon`].
///
/// This is what window managers show in the taskbar/dock and in alt-tab; the
/// desktop-menu entry gets its icon from the installed hicolor theme instead
/// (see `packaging/`). Both come from `packaging/icons/sdroxide.svg`.
#[cfg(not(target_arch = "wasm32"))]
pub fn app_icon() -> eframe::egui::IconData {
    const PNG: &[u8] = include_bytes!("../../../packaging/icons/sdroxide-256.png");
    // Decoding a 256x256 PNG once at startup; a failure here would only cost
    // the icon, so fall back to no icon rather than refusing to open a window.
    match image::load_from_memory_with_format(PNG, image::ImageFormat::Png) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            eframe::egui::IconData { rgba: rgba.into_raw(), width, height }
        }
        Err(e) => {
            eprintln!("sdroxide: decoding the app icon: {e}");
            eframe::egui::IconData::default()
        }
    }
}
