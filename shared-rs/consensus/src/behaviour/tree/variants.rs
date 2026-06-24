//! Consumer-neutral RB-header equivocation variant store.
//!
//! When the BT control signal requests RB-header equivocation
//! (`praos.production = Equivocate{ways}` + `praos.outbound =
//! EquivocateRouting{slot, ways, seed}`), the I/O wrapper produces `ways`
//! distinct RB variants for the slot and records them here. The per-peer send
//! actuator then routes `variant_for(slot, equivocation_bucket(seed, ways,
//! peer))` to each peer, and the block server serves a requested body via
//! `body_for`.
//!
//! The store is **consumer-neutral**: variants are opaque `Vec<u8>` (header +
//! body), so net-rs fills CBOR wire bytes while sim-rs fills its own block
//! representation. It lives in `shared-consensus` (not a single consumer) so
//! both reuse it. It is deliberately *separate* from the per-tick
//! [`ControlSignal`](super::control::ControlSignal): the signal is recomputed
//! and replaced every slot, whereas variants persist across slots (a slow peer
//! may fetch an old variant's body later) — and the bytes are produced by the
//! consumer's I/O layer, which the sans-IO tick cannot do.

use std::collections::BTreeMap;

/// One RB variant produced for an equivocation slot. `hash` is the wire-format
/// header hash (the consumer computes it). `header`/`body` are opaque bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbVariant {
    pub hash: [u8; 32],
    pub header: Vec<u8>,
    pub body: Vec<u8>,
}

/// Slot-keyed store of equivocation variants.
#[derive(Debug, Clone, Default)]
pub struct EquivocationVariants {
    by_slot: BTreeMap<u64, Vec<RbVariant>>,
}

impl EquivocationVariants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the full variant set produced for `slot` (replacing any prior
    /// set for that slot). `variants[0]` is conventionally the primary the
    /// producer adopted locally.
    pub fn record(&mut self, slot: u64, variants: Vec<RbVariant>) {
        self.by_slot.insert(slot, variants);
    }

    /// The variant to route to a peer whose bucket is `bucket`, for `slot`.
    /// `None` if the slot has no recorded variants or the bucket is out of
    /// range (the actuator then sends the honest artefact unchanged).
    pub fn variant_for(&self, slot: u64, bucket: usize) -> Option<&RbVariant> {
        self.by_slot.get(&slot)?.get(bucket)
    }

    /// The body of the variant at `slot` whose header hashes to `hash` — the
    /// block server's fallback when a peer fetches a peer-split variant not in
    /// the local chain store.
    pub fn body_for(&self, slot: u64, hash: &[u8; 32]) -> Option<&[u8]> {
        self.by_slot
            .get(&slot)?
            .iter()
            .find(|v| &v.hash == hash)
            .map(|v| v.body.as_slice())
    }

    /// Number of variants recorded for `slot` (0 if none).
    pub fn ways_at(&self, slot: u64) -> usize {
        self.by_slot.get(&slot).map_or(0, Vec::len)
    }

    /// Drop variants for slots below `slot` (bounded retention).
    pub fn prune_below(&mut self, slot: u64) {
        self.by_slot.retain(|&s, _| s >= slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(tag: u8) -> RbVariant {
        RbVariant {
            hash: [tag; 32],
            header: vec![tag, 0xAA],
            body: vec![tag, 0xBB],
        }
    }

    #[test]
    fn record_and_route_by_bucket() {
        let mut store = EquivocationVariants::new();
        store.record(10, vec![variant(1), variant(2), variant(3)]);
        assert_eq!(store.ways_at(10), 3);
        assert_eq!(store.variant_for(10, 0), Some(&variant(1)));
        assert_eq!(store.variant_for(10, 2), Some(&variant(3)));
        // Out-of-range bucket / unknown slot → None (actuator stays honest).
        assert_eq!(store.variant_for(10, 3), None);
        assert_eq!(store.variant_for(99, 0), None);
    }

    #[test]
    fn body_lookup_by_hash() {
        let mut store = EquivocationVariants::new();
        store.record(10, vec![variant(1), variant(2)]);
        assert_eq!(store.body_for(10, &[2; 32]), Some([2, 0xBB].as_slice()));
        assert_eq!(store.body_for(10, &[9; 32]), None);
        assert_eq!(store.body_for(11, &[1; 32]), None);
    }

    #[test]
    fn prune_below_evicts_old_slots() {
        let mut store = EquivocationVariants::new();
        store.record(5, vec![variant(1)]);
        store.record(10, vec![variant(2)]);
        store.record(15, vec![variant(3)]);
        store.prune_below(10);
        assert_eq!(store.ways_at(5), 0);
        assert_eq!(store.ways_at(10), 1);
        assert_eq!(store.ways_at(15), 1);
    }
}
