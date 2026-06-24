//! Endorser-block "overflow" manifest codec (CIP-0164 prototype).
//!
//! The prototype encodes an EB body as a CBOR map `endorser_block =
//! { tx_hash => tx_size }`, where the key is a 32-byte transaction hash and
//! the value is the transaction's byte size. This is honest wire (de)coding:
//! the producer builds the manifest from its mempool, hashes the bytes to
//! derive the EB key, and downstream nodes decode it to recover the referenced
//! tx hash set in wire order (sizes are ignored — the consumer only needs the
//! hash set + order for bitmap indexing).

use shared_consensus::mempool::TxId;

/// Decode an `endorser_block = { tx_hash => tx_size }` manifest map, returning
/// the referenced tx hashes in wire order. Returns `None` if the blob isn't a
/// well-formed manifest map (32-byte keys, integer values). Sizes are ignored.
pub fn decode_overflow_eb(blob: &[u8]) -> Option<Vec<TxId>> {
    fn read_hash(dec: &mut minicbor::Decoder) -> Option<TxId> {
        let bytes = dec.bytes().ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut h = [0u8; 32];
        h.copy_from_slice(bytes);
        let _size = dec.u32().ok()?; // tx size — unused
        Some(TxId::new_with_array(h))
    }
    let mut dec = minicbor::Decoder::new(blob);
    let entries = dec.map().ok()?;
    let mut hashes = Vec::new();
    match entries {
        Some(n) => {
            hashes.reserve(n as usize);
            for _ in 0..n {
                hashes.push(read_hash(&mut dec)?);
            }
        }
        None => loop {
            // Indefinite-length map: read until the break marker.
            if dec.datatype().ok()? == minicbor::data::Type::Break {
                dec.skip().ok()?;
                break;
            }
            hashes.push(read_hash(&mut dec)?);
        },
    }
    Some(hashes)
}

/// Encode an endorser-block manifest as the prototype
/// `endorser_block = { tx_hash => tx_size }` map. Sizes are `0` (the produce
/// path doesn't track per-tx sizes); the hash order is preserved for bitmap
/// indexing. Pure over the manifest so the caller can hash the bytes to derive
/// the EB key before committing the mempool drain.
pub fn encode_overflow_eb(manifest: &[TxId]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut data);
    let _ = enc.map(manifest.len() as u64);
    for h in manifest {
        let _ = enc.bytes(h.get_bytes()).and_then(|e| e.u32(0));
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blake2b_256;

    #[test]
    fn encode_overflow_eb_is_deterministic() {
        let manifest = vec![
            TxId::new_with_array([0x10u8; 32]),
            TxId::new_with_array([0x20u8; 32]),
        ];
        let a = encode_overflow_eb(&manifest);
        let b = encode_overflow_eb(&manifest);
        assert_eq!(a, b);
        assert_eq!(blake2b_256(&a), blake2b_256(&b));
    }

    #[test]
    fn decode_overflow_eb_round_trip() {
        let manifest = vec![
            TxId::new_with_array([0x10u8; 32]),
            TxId::new_with_array([0x20u8; 32]),
        ];
        let data = encode_overflow_eb(&manifest);
        let hashes = decode_overflow_eb(&data).expect("decode");
        assert_eq!(hashes, manifest);
    }

    #[test]
    fn decode_overflow_eb_rejects_garbage() {
        assert!(decode_overflow_eb(&[0xFF, 0xFF]).is_none());
        assert!(decode_overflow_eb(&[]).is_none());
    }

    #[test]
    fn encode_overflow_eb_layout() {
        let manifest = vec![
            TxId::new_with_array([0xAAu8; 32]),
            TxId::new_with_array([0xBBu8; 32]),
        ];
        let data = encode_overflow_eb(&manifest);
        // Decode the manifest map: { hash => size }, hashes in order.
        let mut dec = minicbor::Decoder::new(&data);
        let n = dec.map().unwrap().unwrap();
        assert_eq!(n, 2);
        assert_eq!(dec.bytes().unwrap(), &[0xAA; 32]);
        assert_eq!(dec.u32().unwrap(), 0);
        assert_eq!(dec.bytes().unwrap(), &[0xBB; 32]);
        assert_eq!(dec.u32().unwrap(), 0);
    }
}
