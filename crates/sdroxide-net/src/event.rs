//! Internal channel vocabulary shared by the feed threads, the worker threads,
//! and the [`crate::SpotManager`].

use crossbeam_channel::Sender;
use sdroxide_types::{CallsignInfo, QsoRecord, Spot, SpotKind, UploadResult};

/// A non-spot result from a network worker, drained by the manager and turned
/// into a `RadioEvent` by the engine.
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// The merged, de-duplicated, age-pruned spot set (manager → engine).
    Spots(Vec<Spot>),
    /// Human-readable feed/connection status (`None` clears it).
    Status(Option<String>),
    /// A callsign-lookup result.
    Callsign(CallsignInfo),
    /// One QSO/target upload result.
    Upload(UploadResult),
    /// Parsed confirmation records downloaded from LoTW/eQSL.
    Confirmations(Vec<QsoRecord>),
}

/// A feed thread's full current view of its own spots (replaces the prior set
/// for that kind in the manager's store).
pub type FeedBatch = (SpotKind, Vec<Spot>);

pub type FeedTx = Sender<FeedBatch>;
pub type EventTx = Sender<NetEvent>;
