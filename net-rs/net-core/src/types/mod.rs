//! Shared Cardano types used across mini-protocols.
//!
//! The ledger-object codec (chain positions, era-tagged headers, block bodies)
//! now lives in the standalone public `net-codec` crate. This module re-exports
//! it under the historical `net_core::types::*` path so the mini-protocol code
//! (and downstream `net_core::types` users) keeps compiling unchanged.
//!
//! - `Point` and `Tip`: chain position types used by ChainSync and BlockFetch.
//! - `WrappedHeader`: era-tagged block headers with optional parsed fields.
//! - `BlockBody`: raw block bodies with optional Leios metadata.

pub use net_codec::{
    decode_points, encode_points, BlockBody, HeaderInfo, LeiosBlockInfo, Point, Tip, Vote,
    WrappedHeader, MAX_BLOCK_SIZE, MAX_HEADER_SIZE, MAX_POINTS,
};
