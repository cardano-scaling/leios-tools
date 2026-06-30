//! Per-peer protocol handling.
//!
//! Manages individual peer connections: connection setup, protocol
//! sub-tasks (initiator, responder, duplex), and server-side handlers.
//! The multi-peer coordination layer lives in the `multi_peer` module.

pub(crate) mod command_dispatch;
pub mod connect;
pub(crate) mod duplex_task;
pub(crate) mod peer_task;
pub mod server_handlers;
pub mod types;

pub use shared_consensus::PeerId;
pub use types::{PeerCommand, PeerEvent};

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// How far a downstream peer has promoted *us* as its upstream, observed from
/// our responder side: `Cold` (connected only) → `Warm` (it's sending us
/// keepalives) → `Hot` (it's pulling our chain via ChainSync/BlockFetch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownstreamState {
    Cold,
    Warm,
    Hot,
}

impl DownstreamState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            2 => Self::Hot,
            1 => Self::Warm,
            _ => Self::Cold,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
            Self::Hot => "hot",
        }
    }
}

/// Shared per-connection downstream-promotion flag (0=cold, 1=warm, 2=hot).
/// Server handlers escalate it with `fetch_max` (monotonic within a
/// connection); a fresh connection allocates a new flag, so reconnects reset to
/// cold. The coordinator reads it when building a peer snapshot — same pattern
/// as the shared `MuxStats` byte counters.
pub type DownstreamFlag = Arc<AtomicU8>;

pub fn new_downstream_flag() -> DownstreamFlag {
    Arc::new(AtomicU8::new(0))
}
/// Escalate to at least Warm (downstream started keepalive).
pub fn mark_downstream_warm(f: &DownstreamFlag) {
    f.fetch_max(1, Ordering::Relaxed);
}
/// Escalate to Hot (downstream is pulling our chain).
pub fn mark_downstream_hot(f: &DownstreamFlag) {
    f.fetch_max(2, Ordering::Relaxed);
}

/// Connection mode determines which protocol roles the peer task runs.
///
/// In Cardano (V10+), TCP direction doesn't restrict protocol roles.
/// Duplex mode runs both initiator and responder protocols on one
/// connection — each protocol ID registered twice, once per mux direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// We initiated TCP, run client-side (initiator) protocols only.
    InitiatorOnly,
    /// They connected to us, run server-side (responder) protocols only.
    ResponderOnly,
    /// Both directions on one connection (not implemented initially).
    Duplex,
}

/// Errors from the peer layer.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("protocol error: {0}")]
    Protocol(#[from] crate::protocols::ProtocolError),

    #[error("mux error: {0}")]
    Mux(#[from] crate::mux::MuxError),

    #[error("connection failed: {0}")]
    Connection(String),

    #[error("coordinator shut down")]
    Shutdown,

    #[error("peer {0} disconnected")]
    Disconnected(PeerId),
}

#[cfg(test)]
mod downstream_tests {
    use super::*;

    #[test]
    fn downstream_flag_escalates_monotonically() {
        let f = new_downstream_flag();
        let read = |f: &DownstreamFlag| DownstreamState::from_u8(f.load(Ordering::Relaxed));
        assert_eq!(read(&f), DownstreamState::Cold);
        mark_downstream_warm(&f);
        assert_eq!(read(&f), DownstreamState::Warm);
        mark_downstream_hot(&f);
        assert_eq!(read(&f), DownstreamState::Hot);
        // Never downgrades: a late warm signal can't undo hot.
        mark_downstream_warm(&f);
        assert_eq!(read(&f), DownstreamState::Hot);
    }

    #[test]
    fn downstream_state_str() {
        assert_eq!(DownstreamState::Cold.as_str(), "cold");
        assert_eq!(DownstreamState::Warm.as_str(), "warm");
        assert_eq!(DownstreamState::Hot.as_str(), "hot");
    }
}
