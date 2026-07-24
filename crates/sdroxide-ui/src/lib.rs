//! The sdroxide GUI: an egui app that talks to any [`sdroxide_types::RadioController`].
//!
//! Compiles native and for wasm32. All custom wgpu rendering that the browser
//! build shares is written to WebGL2 downlevel limits (fragment-only, sampled
//! textures + uniforms). The one exception is [`solar3d`], which is native-only
//! and does its 3D work in an offscreen pass of its own.

mod app;
pub mod chrome;
mod colormap;
mod download;
mod help;
#[cfg(feature = "remote")]
mod remote;
mod rf_paint;
/// Solar-system 3D window. Native-only: it is the sole outbound network client
/// in the UI and the sole consumer of depth/MSAA/vertex buffers.
#[cfg(not(target_arch = "wasm32"))]
mod solar3d;
mod sstv;
pub mod theme;
mod view;
mod waterfall_gpu;
mod widgets;

pub use app::SdroxideApp;
#[cfg(feature = "remote")]
pub use remote::{AudioBridge, RemoteController};

/// Wgpu access must go through this re-export so every crate agrees on the
/// wgpu version (project rule).
pub use eframe::egui_wgpu;
