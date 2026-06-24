# net-codec — Cardano ledger-object CBOR

Public codec crate for Cardano *ledger objects*: chain positions, era-tagged
headers, and block bodies, including their CIP-0164 Leios extensions. All types
carry CBOR codecs for wire compatibility.

This is the **"what the bytes mean"** layer. It is deliberately separate from
net-core's **mini-protocol framing** (the "how messages are multiplexed" layer):
net-codec has no networking, no tokio, no mux. Its only dependency on the rest of
the workspace is `shared-consensus` (for the canonical `Point`/`Tip`/`Vote`
codec). Layering is one-way: `shared-consensus` ← `net-codec` ← `net-core`.

`net-core` re-exports these types under `net_core::types::*`, so existing
mini-protocol code keeps compiling unchanged.

## Files

| File | Description |
|------|-------------|
| `lib.rs` | `Point`, `Tip`, `Vote` (re-exported from `shared-consensus`), CBOR helpers (`encode_points`, `decode_points`), and size constants |
| `header.rs` | `WrappedHeader` (raw CBOR + parsed info), `HeaderInfo` (Shelley+ header fields), header-hash (Blake2b-256) |
| `block.rs` | `BlockBody` (raw CBOR + parsed info), `LeiosBlockInfo` (CIP-0164 EB certificate), `praos_inspect` body parser |
| `eb.rs` | CIP-0164 overflow endorser-block manifest codec (`encode_overflow_eb`, `decode_overflow_eb`) |

## Types

| Type | Description |
|------|-------------|
| `Point` | Chain position: `Origin` or `Specific { slot: u64, hash: [u8; 32] }`. CBOR: origin = `[]`, specific = `[slot, hash]` |
| `Tip` | `Point` + `block_no: u64` — current chain tip position |
| `WrappedHeader` | Raw CBOR bytes (including `#6.24` tag wrapper) + optional parsed `HeaderInfo`. Byron headers return `None` gracefully |
| `HeaderInfo` | Parsed from Shelley+ header body array: `era`, `slot`, `block_number`, `prev_hash`, `issuer_vkey`, `body_size`, `block_body_hash`, plus CIP-0164 extensions (`announced_eb`, `certified_eb`) |
| `BlockBody` | Raw CBOR bytes + optional `LeiosBlockInfo`. MAX_BLOCK_SIZE: 2.5MB |
| `LeiosBlockInfo` | CIP-0164: extracted EB certificate bytes from block body (field 4 of Shelley+ block array, if present) |

## Header Parsing

`HeaderInfo` dispatches on the CBOR datatype of each trailing field (not array
length alone) for the optional Leios extensions, to stay liberal about the
disagreement between CIP-0164 drafts on how `announced_eb` is grouped:

- `bool` → `certified_eb`
- `array(2)` → `announced_eb` tuple `[hash, size]`
- `bytes(32)` followed by a `u32` → flat `announced_eb`

Parsing is best-effort and silent — unknown formats return `None` rather than
errors.

## Security

The header/block parsers take **untrusted wire input**. Every length read from
the wire is bounded before allocation (`MAX_POINTS`, `MAX_HEADER_SIZE`,
`MAX_BLOCK_SIZE`, and the per-decoder array-length checks), per the net-rs
"Security audit" discipline. When changing CBOR encoding, capture real bytes
with `net-cli capture` and add them as `const` test vectors (see the inline
`#[cfg(test)]` modules in `header.rs` / `block.rs`).

## Constants

- `MAX_POINTS`: 2048 — maximum points in a FindIntersect request
- `MAX_HEADER_SIZE`: 65,535 bytes
- `MAX_BLOCK_SIZE`: 2,500,000 bytes
