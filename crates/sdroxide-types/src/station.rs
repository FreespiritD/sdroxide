//! The persisted configuration that belongs to the *station* rather than to
//! the screen in front of it.
//!
//! Five files, all written next to each other in the config directory and all
//! owned by the engine: the network cockpit (`net.json`), the two built-in
//! control servers (`rigctld.json`, `tciserver.json`), the WSJT-X UDP
//! broadcast (`wsjtx.json`) and the operator's satellite additions
//! (`satellites.json`).
//!
//! They are bundled into one announcement because they answer one question —
//! "what is this station set up to do?" — and because a remote client has to
//! be *told*: it has no access to the machine the engine runs on, so a settings
//! dialog there would otherwise open on defaults and, worse, could write those
//! defaults back over the operator's real configuration. The engine emits this
//! once at startup and again after every change, so the answer is always the
//! current one, whichever screen asked.
//!
//! What is *not* here: anything describing the operator's own desk. The
//! control-input bindings and the UI settings stay client-side, because a knob
//! on this table has nothing to do with the radio in the other room.

use serde::{Deserialize, Serialize};

use crate::{NetworkConfig, RigctldConfig, SatConfig, TciServerConfig, WsjtxConfig};

/// Everything the engine host persists on the station's behalf, as one
/// snapshot. See the module docs for why these five travel together.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StationConfig {
    /// Spot feeds, callsign lookup, uploads and the credentials they need.
    pub net: NetworkConfig,
    /// The built-in Hamlib rigctld listener.
    pub rigctld: RigctldConfig,
    /// The built-in TCI listener.
    pub tci_server: TciServerConfig,
    /// The WSJT-X UDP broadcast.
    pub wsjtx: WsjtxConfig,
    /// Element sets the operator added, the listings they subscribe to, and
    /// their satellite frequency overrides.
    pub sat: SatConfig,
}
