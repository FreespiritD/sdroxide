//! FT8/FT4 digital-mode engine for sdroxide.
//!
//! **Licensing:** this crate links `mfsk-core` (GPL-3.0-or-later); it is the
//! only crate in the workspace that does. It is used only in the native
//! binary — the wasm remote client links none of it (all decode/encode runs
//! server-side).

pub mod controller;
pub mod fsq_controller;
pub mod modem;
pub mod params;
pub mod qso;
pub mod rade_controller;
pub mod rf_paint_controller;
pub mod scheduler;
pub mod squelch;
pub mod sstv_controller;
pub mod text_modem;

pub use controller::{DigiAction, DigiController};
pub use fsq_controller::FsqController;
pub use rade_controller::RadeController;
pub use rf_paint_controller::RfPaintController;
pub use sstv_controller::SstvController;
pub use modem::Ft8Modem;
pub use params::{DECODE_RATE, DigiParams};
pub use qso::QsoMachine;
pub use scheduler::SlotScheduler;
pub use text_modem::TextModemController;

use std::time::SystemTime;

use sdroxide_types::{DigiConfig, DigiStatus, Mode, SstvMode};

/// The engine-facing digital-mode seam, implemented by the slotted FT8/FT4
/// [`DigiController`] and the continuous-keyboard [`TextModemController`]. The
/// engine holds one as `Box<dyn DigiEngine>` and never branches on the mode.
///
/// Method-syntax note: the FT8 controller keeps inherent methods of the same
/// names, so its trait impl delegates with fully-qualified calls.
pub trait DigiEngine: Send {
    fn mode(&self) -> Mode;
    fn on_rx_audio(&mut self, tap: &[f32]);
    fn poll(&mut self, now: SystemTime, dial_hz: f64) -> Vec<DigiAction>;
    fn tx_burst_active(&self) -> bool;
    fn fill_tx_block(&mut self, out: &mut [f32]) -> bool;
    fn on_burst_done(&mut self);
    fn abort(&mut self);
    fn abort_tx(&mut self);
    fn set_config(&mut self, cfg: DigiConfig);
    fn set_audio_hz(&mut self, hz: f32);
    fn audio_hz(&self) -> f32;
    fn status(&self) -> DigiStatus;

    // Actions that only some modes use; default to no-ops.
    fn call_cq(&mut self) {}
    fn start_qso(
        &mut self,
        _from: String,
        _grid: Option<String>,
        _snr: i16,
        _audio_hz: f32,
        _wait_for_cq: bool,
    ) {
    }
    fn stop_qso(&mut self) {}
    /// FT8/FT4: pick which message goes out next (the operator's Tx1–Tx6).
    fn set_step(&mut self, _step: sdroxide_types::QsoStep) {}
    /// FT8/FT4: queue a message to send verbatim in the next transmit slot.
    fn send_text(&mut self, _text: String) {}
    /// Continuous keyboard modes: replace the outgoing text buffer.
    fn set_tx_text(&mut self, _text: String) {}
    /// Continuous keyboard modes: enter/leave transmit.
    fn set_tx_active(&mut self, _on: bool) {}
    /// SSTV: select the mode (`None` = auto-detect on RX, Martin 1 on TX).
    fn set_sstv_mode(&mut self, _mode: Option<SstvMode>) {}
    /// SSTV: queue a composed image (interleaved RGB) and start transmitting.
    fn set_sstv_image(&mut self, _mode: SstvMode, _rgb: Vec<u8>, _w: u16, _h: u16) {}
    /// FSQ image: queue a grayscale image (`w*h` bytes) and start transmitting.
    fn set_image(&mut self, _gray: Vec<u8>, _w: u16, _h: u16) {}

    // --- digital voice ---
    //
    // The text and image modes are decoded *from* the receive audio and
    // transmitted *as* a synthesised burst. Digital voice is neither: it
    // produces receive audio of its own, and it transmits the live microphone.
    // These three hooks are the whole difference, and default to inert.

    /// Take decoded speech at 48 kHz, appending to `out`.
    ///
    /// `true` means this mode is producing audio and the engine should play it
    /// instead of the demodulated signal; `false` leaves the normal audio path
    /// alone (so an out-of-sync RADE receiver still passes the raw SSB through,
    /// unless [`DigiEngine::mutes_analog_audio`] says otherwise).
    fn rx_audio_out(&mut self, _out: &mut Vec<f32>) -> bool {
        false
    }

    /// True when the mode wants the demodulated (analog) audio silenced rather
    /// than passed through, so only what it decodes is audible. Consulted only
    /// where [`DigiEngine::rx_audio_out`] declined — decoded audio always wins.
    fn mutes_analog_audio(&self) -> bool {
        false
    }

    /// True for modes that transmit live microphone audio, so the engine keeps
    /// the mic alive during transmit instead of discarding it.
    fn wants_mic(&self) -> bool {
        false
    }

    /// Microphone audio at 48 kHz, delivered only while [`DigiEngine::wants_mic`].
    fn on_tx_mic(&mut self, _mic_48k: &[f32]) {}
}
