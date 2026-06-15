//! Raw block bodies with optional Leios metadata (CIP-0164).

use std::sync::atomic::{AtomicUsize, Ordering};

use minicbor::decode::Error as DecodeError;
use minicbor::encode::Error as EncodeError;
use minicbor::{Decoder, Encoder};
use tracing::warn;

use shared_consensus::praos::{LeiosCertSummary, ParsedBodyInfo};

use super::{Point, MAX_BLOCK_SIZE};

/// Number of base fields in a Shelley+ block array (before Leios extensions).
const BLOCK_BASE_FIELDS: u64 = 4;

/// Cap on the byte prefix captured when a `leios_certificate` shape
/// probe fails.  Enough to identify the CBOR major type, top-level
/// array length, and the first few elements when shape-comparing
/// against draft revisions of CIP-0164.
const UNKNOWN_CERT_FIELD_PROBE_BYTES: usize = 96;

/// Soft cap on the number of `praos_inspect` shape-mismatch WARN lines
/// emitted per process.  Counter is process-global rather than
/// per-peer; the diagnostic exists to identify the prototype's wire
/// shape on first contact, not to repeat the same hex every block.
const SHAPE_MISMATCH_WARN_BUDGET: usize = 5;

/// Soft cap on the number of `praos_inspect` parse-failure WARN lines
/// emitted per process.  Without this, `praos_inspect()` silently
/// returns `ParsedBodyInfo::default()` on any decode error and the
/// failure is invisible — see the dev-relay's post-slot-1309596
/// EB-announcing blocks for the original motivation.
const BODY_PARSE_FAIL_WARN_BUDGET: usize = 5;

/// Bytes to capture from the start of a failing body for hex dump.
/// Large enough to expose the CBOR tag, era field, top-level array
/// length, and the first few merged_block fields — enough to identify
/// an unrecognised wire shape on first contact.
const BODY_PARSE_FAIL_PROBE_BYTES: usize = 256;

static SHAPE_MISMATCH_WARNS: AtomicUsize = AtomicUsize::new(0);
static BODY_PARSE_FAIL_WARNS: AtomicUsize = AtomicUsize::new(0);

// --- LeiosBlockInfo ---

/// Leios metadata parsed from a block body (CIP-0164).
///
/// The EB certificate is extracted from real blocks received via BlockFetch.
/// The Shelley+ block structure is:
///   `#6.24(bytes .cbor [era_tag, [header, tx_bodies, tx_witnesses, aux_data, ?eb_certificate]])`
/// Base field count = 4; a 5th element is the EB certificate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeiosBlockInfo {
    /// Opaque EB certificate bytes, if present in this block.
    pub eb_certificate: Option<Vec<u8>>,
}

impl LeiosBlockInfo {
    /// Try to parse Leios metadata from raw BlockBody bytes.
    ///
    /// Returns None for Byron blocks or blocks without an EB certificate.
    /// Parsing failures are silent — this is best-effort extraction.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        Self::try_parse(raw).ok()
    }

    fn try_parse(raw: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(raw);

        // Unwrap #6.24 tag
        let tag = d.tag()?;
        if tag.as_u64() != 24 {
            return Err(DecodeError::message(format!(
                "expected CBOR tag 24, got {}",
                tag.as_u64()
            )));
        }
        let inner_bytes = d.bytes()?;

        // Inner: [era_tag, era_block]
        let mut inner = Decoder::new(inner_bytes);
        let _outer_len = inner.array()?;
        let era = inner.u32()?;

        // Byron (era 0 or 1) — no Leios support
        if era < 2 {
            return Err(DecodeError::message("Byron block"));
        }

        // era_block: [header, tx_bodies, tx_witnesses, aux_data, ?eb_certificate]
        let block_len = match inner.array()? {
            Some(n) => n,
            None => return Err(DecodeError::message("indefinite block array")),
        };

        if block_len <= BLOCK_BASE_FIELDS {
            return Err(DecodeError::message("no Leios extension fields"));
        }

        // Skip base fields 0-3
        for _ in 0..BLOCK_BASE_FIELDS {
            inner.skip()?;
        }

        // Field 4: eb_certificate — extract as opaque bytes
        let cert_start = inner.position();
        inner.skip()?;
        let cert_end = inner.position();
        let cert_bytes = inner_bytes
            .get(cert_start..cert_end)
            .ok_or_else(|| DecodeError::message("failed to extract certificate bytes"))?;

        Ok(LeiosBlockInfo {
            eb_certificate: Some(cert_bytes.to_vec()),
        })
    }
}

// --- BlockBody ---

/// A full block stored as raw CBOR bytes (including the #6.24 tag wrapper),
/// with optional parsed Leios metadata.
///
/// For Shelley+ blocks with a CIP-0164 EB certificate, `leios` contains the
/// extracted certificate bytes. For blocks without one, `leios` is None.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockBody {
    /// Raw CBOR bytes of the block.
    pub raw: Vec<u8>,
    /// Parsed Leios metadata (Shelley+ only). None if no certificate or parse failure.
    pub leios: Option<LeiosBlockInfo>,
}

impl BlockBody {
    /// Create a BlockBody from raw bytes, attempting to parse Leios metadata.
    pub fn new(raw: Vec<u8>) -> Self {
        let leios = LeiosBlockInfo::parse(&raw);
        Self { raw, leios }
    }

    /// Create a BlockBody from raw bytes without parsing.
    /// Use for test fixtures with trivial CBOR that isn't a real block.
    pub fn opaque(raw: Vec<u8>) -> Self {
        Self { raw, leios: None }
    }

    /// Derive the chain Point (slot + header hash) from this block's raw bytes.
    ///
    /// Extracts the header from the block, parses it for the slot number,
    /// and computes Blake2b-256 of the header CBOR for the block hash.
    /// Returns None for Byron blocks or unparseable data.
    pub fn point(&self) -> Option<Point> {
        self.try_point().ok()
    }

    /// Extract the header from this block body as a WrappedHeader.
    ///
    /// Returns None for Byron blocks or unparseable data.
    pub fn header(&self) -> Option<super::WrappedHeader> {
        let buf = self.try_extract_header().ok()?;
        Some(super::WrappedHeader::new(buf))
    }

    /// One-pass inspection of a Conway+ Praos `merged_block`.
    ///
    /// Returns a populated [`ParsedBodyInfo`] when the block parses;
    /// `ParsedBodyInfo::default()` (i.e. all zeros / `None`) on any
    /// decode failure (Byron, opaque test fixtures, malformed CBOR).
    ///
    /// CIP-0164's CDDL for `merged_block` (with the Conway-era
    /// `invalid_transactions` field that the CIP text currently omits):
    ///
    /// ```text
    /// merged_block = [
    ///   header,
    ///   transaction_bodies,
    ///   transaction_witness_sets,
    ///   auxiliary_data_set,
    ///   invalid_transactions,
    ///   ? eb_certificate,
    ///   ? eb_tx_references,
    /// ]
    /// ```
    ///
    /// Field-count mapping:
    ///   5 — base Conway body, no Leios extensions
    ///   6 — first trailing optional present (CDDL positions it as
    ///       `eb_certificate`)
    ///   7 — both trailing optionals present
    ///
    /// `leios_cert` is `Some` iff field 5 (after the base) decodes as
    /// an `array(4)` whose first element is a `uint` — i.e. matches the
    /// CIP-0164 `leios_certificate = [slot_no, endorser_block_hash,
    /// signers, aggregated_signature]` shape.  Only `slot_no` and
    /// `endorser_block_hash` are surfaced; the bitfield and BLS
    /// signature stay in the raw bytes.
    ///
    /// `tx_references_count` is `Some(n)` iff field 6 decodes as an
    /// array of length `n` (the CDDL `[* tx_reference]`).
    pub fn praos_inspect(&self) -> ParsedBodyInfo {
        match self.try_praos_inspect() {
            Ok(info) => info,
            Err(e) => {
                // `praos_inspect()`'s contract is best-effort, but a
                // body we can't decode is otherwise invisible to
                // operators (returns `field_count=0` and looks like an
                // empty block).  Dump a hex prefix so an unrecognised
                // wire shape can be identified on first contact.
                // Throttled like the trailing-optional shape mismatch
                // above — process-global, not per-peer.
                if BODY_PARSE_FAIL_WARNS.fetch_add(1, Ordering::Relaxed)
                    < BODY_PARSE_FAIL_WARN_BUDGET
                {
                    let take = self.raw.len().min(BODY_PARSE_FAIL_PROBE_BYTES);
                    warn!(
                        body_bytes = self.raw.len(),
                        error = %e,
                        raw_prefix_hex = %hex_prefix(Some(&self.raw[..take])),
                        "praos body parse failed; dumping raw prefix"
                    );
                }
                ParsedBodyInfo::default()
            }
        }
    }

    fn try_praos_inspect(&self) -> Result<ParsedBodyInfo, DecodeError> {
        let mut d = Decoder::new(&self.raw);

        let tag = d.tag()?;
        if tag.as_u64() != 24 {
            return Err(DecodeError::message("expected CBOR tag 24"));
        }
        let inner_bytes = d.bytes()?;

        let mut inner = Decoder::new(inner_bytes);
        let _outer_len = inner.array()?;
        let era = inner.u32()?;
        if era < 2 {
            return Err(DecodeError::message("Byron block"));
        }
        let block_len = match inner.array()? {
            Some(n) => n,
            None => return Err(DecodeError::message("indefinite block array")),
        };
        let field_count = u32::try_from(block_len)
            .map_err(|_| DecodeError::message("merged_block length exceeds u32"))?;
        if block_len < 2 {
            return Ok(ParsedBodyInfo {
                field_count,
                ..ParsedBodyInfo::default()
            });
        }

        // Field 0: header — skip.
        inner.skip()?;
        // Field 1: tx_bodies — `[* transaction_body]`.
        let tx_count = match inner.array()? {
            Some(n) => u32::try_from(n)
                .map_err(|_| DecodeError::message("tx_bodies length exceeds u32"))?,
            None => return Err(DecodeError::message("indefinite tx_bodies array")),
        };
        for _ in 0..tx_count {
            inner.skip()?;
        }
        // Skip the rest of the Conway base: tx_witness_sets,
        // auxiliary_data_set, invalid_transactions.  We treat the count
        // permissively to keep working if the era/CDDL adds another
        // mandatory field — at worst the trailing-optional probes below
        // see the wrong field and bail to `None`, never corrupt state.
        let base_remaining = block_len.saturating_sub(2).min(3);
        for _ in 0..base_remaining {
            inner.skip()?;
        }

        let trailing = block_len.saturating_sub(2 + base_remaining);

        let mut leios_cert = None;
        let mut tx_references_count = None;
        let mut cert_field_bytes: Option<Vec<u8>> = None;
        let mut tx_refs_field_bytes: Option<Vec<u8>> = None;

        // Trailing optional 1: try as leios_certificate.  On a shape
        // mismatch (e.g. the prototype's `leios_certificate` doesn't
        // match the CIP-0164 CDDL we ship) capture the raw bytes so
        // we can WARN about them below.
        if trailing >= 1 {
            let start = inner.position();
            match try_decode_leios_cert(&mut inner) {
                Ok(c) => leios_cert = Some(c),
                Err(_) => {
                    inner.set_position(start);
                    inner.skip()?;
                    let end = inner.position();
                    let raw = &inner.input()[start..end];
                    let take = raw.len().min(UNKNOWN_CERT_FIELD_PROBE_BYTES);
                    cert_field_bytes = Some(raw[..take].to_vec());
                }
            }
        }

        // Trailing optional 2: tx_references as `[* tx_reference]`.
        // Same shape-or-capture treatment.
        if trailing >= 2 {
            let start = inner.position();
            match inner.array() {
                Ok(Some(n)) => {
                    tx_references_count =
                        Some(u32::try_from(n).unwrap_or(u32::MAX));
                    for _ in 0..n {
                        inner.skip().ok();
                    }
                }
                Ok(None) => {
                    // Indefinite array — count via skip-until-break.
                    let mut count: u32 = 0;
                    while inner.datatype()? != minicbor::data::Type::Break {
                        inner.skip()?;
                        count = count.saturating_add(1);
                    }
                    inner.skip()?; // break
                    tx_references_count = Some(count);
                }
                Err(_) => {
                    inner.set_position(start);
                    inner.skip()?;
                    let end = inner.position();
                    let raw = &inner.input()[start..end];
                    let take = raw.len().min(UNKNOWN_CERT_FIELD_PROBE_BYTES);
                    tx_refs_field_bytes = Some(raw[..take].to_vec());
                }
            }
        }

        // Codec-level diagnostic: if either trailing field didn't match
        // the expected shape, dump a hex prefix.  Throttled across the
        // process so a chain full of mismatched blocks logs the first
        // few only.
        if (cert_field_bytes.is_some() || tx_refs_field_bytes.is_some())
            && SHAPE_MISMATCH_WARNS.fetch_add(1, Ordering::Relaxed) < SHAPE_MISMATCH_WARN_BUDGET
        {
            warn!(
                field_count,
                cert_field_hex = %hex_prefix(cert_field_bytes.as_deref()),
                tx_refs_field_hex = %hex_prefix(tx_refs_field_bytes.as_deref()),
                "praos body trailing optional did not match CIP-0164 shape"
            );
        }

        Ok(ParsedBodyInfo {
            tx_count,
            field_count,
            leios_cert,
            tx_references_count,
        })
    }

    /// Extract the header from this block in ChainSync wire format:
    /// `[era_tag, #6.24(header_cbor)]`.
    ///
    /// Inside the block, the header is stored as raw CBOR `[header_body, sig]`.
    /// This method wraps it in `#6.24` to match the ChainSync wire format,
    /// ensuring consistent hashing and downstream compatibility.
    fn try_extract_header(&self) -> Result<Vec<u8>, DecodeError> {
        let mut d = Decoder::new(&self.raw);

        // Unwrap #6.24 tag
        let tag = d.tag()?;
        if tag.as_u64() != 24 {
            return Err(DecodeError::message("expected CBOR tag 24"));
        }
        let inner_bytes = d.bytes()?;

        // Inner: [era_tag, era_block]
        let mut inner = Decoder::new(inner_bytes);
        let _outer_len = inner.array()?;
        let era = inner.u32()?;

        if era < 2 {
            return Err(DecodeError::message("Byron block"));
        }

        // era_block: [header, tx_bodies, ...]
        // Record position before/after header to extract its raw bytes.
        let _block_len = inner.array()?;
        let header_start = inner.position();
        inner.skip()?; // skip header
        let header_end = inner.position();

        let header_inner_bytes = inner_bytes
            .get(header_start..header_end)
            .ok_or_else(|| DecodeError::message("failed to extract header bytes"))?;

        // Reconstruct in ChainSync wire format: [era_tag, #6.24(header_cbor)]
        let mut header_buf = Vec::new();
        let mut he = Encoder::new(&mut header_buf);
        he.array(2)
            .map_err(|_| DecodeError::message("encode error"))?;
        he.u32(era)
            .map_err(|_| DecodeError::message("encode error"))?;
        he.tag(minicbor::data::Tag::new(24))
            .map_err(|_| DecodeError::message("encode error"))?;
        he.bytes(header_inner_bytes)
            .map_err(|_| DecodeError::message("encode error"))?;

        Ok(header_buf)
    }

    fn try_point(&self) -> Result<Point, DecodeError> {
        let header_buf = self.try_extract_header()?;

        // Parse header for slot.
        let info = super::HeaderInfo::parse(&header_buf)
            .ok_or_else(|| DecodeError::message("failed to parse header"))?;

        // Compute Blake2b-256 of the full header CBOR for the block hash.
        let hash = super::header::header_hash(&header_buf);

        Ok(Point::Specific {
            slot: info.slot,
            hash,
        })
    }
}

impl minicbor::Encode<()> for BlockBody {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), EncodeError<W::Error>> {
        e.writer_mut()
            .write_all(&self.raw)
            .map_err(EncodeError::write)?;
        Ok(())
    }
}

impl<'a> minicbor::Decode<'a, ()> for BlockBody {
    fn decode(d: &mut Decoder<'a>, _ctx: &mut ()) -> Result<Self, DecodeError> {
        let start = d.position();
        d.skip()?;
        let end = d.position();
        let len = end - start;
        if len > MAX_BLOCK_SIZE {
            return Err(DecodeError::message(format!(
                "block too large: {len} bytes exceeds limit {MAX_BLOCK_SIZE}"
            )));
        }
        let raw = d
            .input()
            .get(start..end)
            .ok_or_else(|| DecodeError::message("failed to extract block bytes"))?;
        Ok(BlockBody::new(raw.to_vec()))
    }
}

/// Format up to `UNKNOWN_CERT_FIELD_PROBE_BYTES` of raw bytes as a
/// lowercase hex string, or `"none"` when the slot wasn't captured.
fn hex_prefix(bytes: Option<&[u8]>) -> String {
    use std::fmt::Write as _;
    match bytes {
        Some(b) => {
            let mut s = String::with_capacity(b.len() * 2);
            for x in b {
                let _ = write!(s, "{x:02x}");
            }
            s
        }
        None => "none".to_string(),
    }
}

/// Attempt to decode the next CBOR element as a CIP-0164
/// `leios_certificate = [slot_no, endorser_block_hash, signers,
/// aggregated_signature]`.  Returns the summary on a shape match;
/// `Err` on anything else (e.g. an `eb_tx_references` array in this
/// slot instead) so the caller's outer Result coerces to `None`.
fn try_decode_leios_cert(d: &mut Decoder<'_>) -> Result<LeiosCertSummary, DecodeError> {
    match d.array()? {
        Some(4) => {}
        Some(other) => {
            return Err(DecodeError::message(format!(
                "leios_certificate expected array(4), got array({other})"
            )));
        }
        None => {
            return Err(DecodeError::message(
                "leios_certificate indefinite array not supported",
            ));
        }
    }
    let eb_slot = d.u64()?;
    let eb_hash_bytes = d.bytes()?;
    if eb_hash_bytes.len() != 32 {
        return Err(DecodeError::message(format!(
            "leios_certificate eb_hash expected 32 bytes, got {}",
            eb_hash_bytes.len()
        )));
    }
    let mut eb_hash = [0u8; 32];
    eb_hash.copy_from_slice(eb_hash_bytes);
    // Skip signers (bytes) and aggregated_signature.
    d.skip()?;
    d.skip()?;
    Ok(LeiosCertSummary { eb_slot, eb_hash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicbor::Encoder;

    /// Build a fake Shelley+ block body for testing.
    /// Produces: #6.24(bytes .cbor [era_tag, [header, txs, witnesses, aux, ?cert]])
    fn build_test_block(era: u8, eb_certificate: Option<&[u8]>) -> Vec<u8> {
        build_test_block_with_tx_count(era, 0, eb_certificate)
    }

    /// Like `build_test_block` but with a configurable `tx_bodies` array length.
    fn build_test_block_with_tx_count(
        era: u8,
        tx_count: u64,
        eb_certificate: Option<&[u8]>,
    ) -> Vec<u8> {
        use std::io::Write as _;
        let field_count = BLOCK_BASE_FIELDS + if eb_certificate.is_some() { 1 } else { 0 };

        // Build inner block array: [header, txs, witnesses, aux, ?cert]
        let mut block_buf = Vec::new();
        let mut be = Encoder::new(&mut block_buf);
        be.array(field_count).unwrap();
        be.bytes(&[0x80]).unwrap(); // dummy header
        be.array(tx_count).unwrap(); // tx_bodies (variable length, empty entries)
        for _ in 0..tx_count {
            be.null().unwrap();
        }
        be.array(0).unwrap(); // empty tx_witnesses
        be.null().unwrap(); // null auxiliary_data
        if let Some(cert) = eb_certificate {
            be.bytes(cert).unwrap();
        }

        // Build outer: [era_tag, block_array]
        let mut inner_buf = Vec::new();
        let mut ie = Encoder::new(&mut inner_buf);
        ie.array(2).unwrap();
        ie.u32(era as u32).unwrap();
        ie.writer_mut().write_all(&block_buf).unwrap();

        // Wrap in #6.24
        let mut outer_buf = Vec::new();
        let mut oe = Encoder::new(&mut outer_buf);
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner_buf).unwrap();

        outer_buf
    }

    #[test]
    fn block_body_round_trip() {
        // Simulate #6.24(bytes): CBOR tag 24 wrapping some bytes.
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.tag(minicbor::data::Tag::new(24)).unwrap();
        e.bytes(&[0x01, 0x02, 0x03]).unwrap();

        let body = BlockBody::opaque(buf.clone());
        let encoded = minicbor::to_vec(&body).unwrap();
        assert_eq!(encoded, buf);

        let decoded: BlockBody = minicbor::decode(&encoded).unwrap();
        assert_eq!(decoded.raw, buf);
    }

    #[test]
    fn parse_block_body_no_certificate() {
        let raw = build_test_block(7, None);
        assert!(LeiosBlockInfo::parse(&raw).is_none());
    }

    #[test]
    fn parse_block_body_with_certificate() {
        let cert_data = vec![0xCA, 0xFE, 0xBA, 0xBE];
        let raw = build_test_block(7, Some(&cert_data));
        let info = LeiosBlockInfo::parse(&raw).expect("should parse");
        let cert = info.eb_certificate.expect("should have certificate");
        // Certificate is stored as an opaque CBOR span (bytes item with header).
        // Verify the content is there by decoding the bstr.
        let mut d = Decoder::new(&cert);
        let decoded = d.bytes().unwrap();
        assert_eq!(decoded, &cert_data);
    }

    #[test]
    fn parse_block_body_byron_returns_none() {
        let raw = build_test_block(0, None);
        assert!(LeiosBlockInfo::parse(&raw).is_none());
    }

    #[test]
    fn parse_block_body_invalid_returns_none() {
        assert!(LeiosBlockInfo::parse(&[0xFF]).is_none());
        assert!(LeiosBlockInfo::parse(&[]).is_none());
    }

    #[test]
    fn block_body_new_parses_certificate() {
        let cert_data = vec![0x01, 0x02, 0x03];
        let raw = build_test_block(7, Some(&cert_data));
        let body = BlockBody::new(raw);
        assert!(body.leios.is_some());
        assert!(body.leios.unwrap().eb_certificate.is_some());
    }

    #[test]
    fn block_body_opaque_skips_parsing() {
        let raw = build_test_block(7, Some(&[0x01]));
        let body = BlockBody::opaque(raw);
        assert!(body.leios.is_none());
    }

    /// Build a block with a real parseable Shelley+ header for point() testing.
    fn build_block_with_header(era: u8, slot: u64) -> Vec<u8> {
        use std::io::Write as _;

        // Build header_body: [block_number, slot, prev_hash, issuer_vkey,
        //   vrf_vkey, vrf_result, body_size, block_body_hash, op_cert, proto_ver]
        let mut hb_buf = Vec::new();
        let mut hb = Encoder::new(&mut hb_buf);
        hb.array(10).unwrap();
        hb.u64(42).unwrap(); // block_number
        hb.u64(slot).unwrap(); // slot
        hb.bytes(&[0xAA; 32]).unwrap(); // prev_hash
        hb.bytes(&[0xBB; 32]).unwrap(); // issuer_vkey
        hb.bytes(&[0u8; 32]).unwrap(); // vrf_vkey
        hb.array(2).unwrap(); // vrf_result
        hb.bytes(&[0u8; 32]).unwrap();
        hb.bytes(&[0u8; 32]).unwrap();
        hb.u32(1024).unwrap(); // body_size
        hb.bytes(&[0xCC; 32]).unwrap(); // block_body_hash
        hb.array(4).unwrap(); // op_cert
        hb.bytes(&[0u8; 32]).unwrap();
        hb.u64(0).unwrap();
        hb.u64(0).unwrap();
        hb.bytes(&[0u8; 64]).unwrap();
        hb.array(2).unwrap(); // proto_ver
        hb.u32(10).unwrap();
        hb.u32(0).unwrap();

        // Build header: [header_body, body_signature]
        let mut header_buf = Vec::new();
        let mut hi = Encoder::new(&mut header_buf);
        hi.array(2).unwrap();
        hi.writer_mut().write_all(&hb_buf).unwrap();
        hi.bytes(&[0u8; 64]).unwrap(); // dummy signature

        // Block array: [header, txs, witnesses, aux]
        // Note: real Cardano blocks store the header directly (no #6.24 wrapping).
        let mut block_buf = Vec::new();
        let mut be = Encoder::new(&mut block_buf);
        be.array(4).unwrap();
        be.writer_mut().write_all(&header_buf).unwrap();
        be.array(0).unwrap(); // txs
        be.array(0).unwrap(); // witnesses
        be.null().unwrap(); // aux

        // Outer: [era_tag, block_array]
        let mut inner_buf = Vec::new();
        let mut ie = Encoder::new(&mut inner_buf);
        ie.array(2).unwrap();
        ie.u32(era as u32).unwrap();
        ie.writer_mut().write_all(&block_buf).unwrap();

        // Wrap in #6.24
        let mut outer_buf = Vec::new();
        let mut oe = Encoder::new(&mut outer_buf);
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner_buf).unwrap();

        outer_buf
    }

    #[test]
    fn block_body_point_extracts_slot_and_hash() {
        let raw = build_block_with_header(7, 67890);
        let body = BlockBody::new(raw);
        let point = body
            .point()
            .expect("should derive point from Shelley+ block");
        match point {
            Point::Specific { slot, hash } => {
                assert_eq!(slot, 67890);
                // Hash should be Blake2b-256 of the reconstructed header CBOR.
                // Just verify it's nonzero (deterministic but hard to precompute).
                assert_ne!(hash, [0u8; 32]);
            }
            Point::Origin => panic!("expected Specific point"),
        }
    }

    #[test]
    fn block_body_header_extracts_matching_point() {
        let raw = build_block_with_header(7, 99999);
        let body = BlockBody::new(raw);
        let header = body.header().expect("should extract header");
        let body_point = body.point().expect("should derive point");
        let header_point = header.point().expect("header should have point");
        assert_eq!(body_point, header_point);
    }

    #[test]
    fn block_body_header_byron_returns_none() {
        let raw = build_test_block(0, None);
        let body = BlockBody::new(raw);
        assert!(body.header().is_none());
    }

    #[test]
    fn block_body_point_byron_returns_none() {
        let raw = build_test_block(0, None);
        let body = BlockBody::new(raw);
        assert!(body.point().is_none());
    }

    #[test]
    fn block_body_point_invalid_returns_none() {
        let body = BlockBody::opaque(vec![0xFF]);
        assert!(body.point().is_none());
    }

    #[test]
    fn praos_inspect_empty() {
        let raw = build_test_block_with_tx_count(7, 0, None);
        let body = BlockBody::new(raw);
        let info = body.praos_inspect();
        assert_eq!(info.tx_count, 0);
        // build_test_block_with_tx_count emits the legacy 4-field base
        // (no `invalid_transactions`); good enough as a control case.
        assert_eq!(info.field_count, 4);
        assert!(info.leios_cert.is_none());
        assert!(info.tx_references_count.is_none());
    }

    #[test]
    fn praos_inspect_several() {
        let raw = build_test_block_with_tx_count(7, 5, None);
        let body = BlockBody::new(raw);
        assert_eq!(body.praos_inspect().tx_count, 5);
    }

    #[test]
    fn praos_inspect_with_eb_certificate_blob() {
        // The fixture emits a 2-byte opaque cert blob, so the cert
        // shape probe doesn't match `[slot, hash, signers, sig]` and
        // we report `leios_cert = None`.  The tx_count still extracts.
        let raw = build_test_block_with_tx_count(7, 3, Some(&[0xCA, 0xFE]));
        let body = BlockBody::new(raw);
        let info = body.praos_inspect();
        assert_eq!(info.tx_count, 3);
        assert_eq!(info.field_count, 5);
        assert!(info.leios_cert.is_none());
    }

    #[test]
    fn praos_inspect_with_real_leios_certificate_shape() {
        // Conway-era body layout: 5 base fields including
        // `invalid_transactions`, then a `leios_certificate` trailing
        // optional encoded inline as an `array(4)`.
        use std::io::Write as _;
        let tx_count = 2u64;
        let mut block_buf = Vec::new();
        let mut be = Encoder::new(&mut block_buf);
        be.array(6).unwrap(); // 5 Conway base + 1 cert
        be.bytes(&[0x80]).unwrap(); // 0 header
        be.array(tx_count).unwrap(); // 1 tx_bodies
        for _ in 0..tx_count {
            be.null().unwrap();
        }
        be.array(0).unwrap(); // 2 tx_witness_sets
        be.null().unwrap(); // 3 auxiliary_data_set
        be.array(0).unwrap(); // 4 invalid_transactions
        // 5 leios_certificate
        be.array(4).unwrap();
        be.u64(12345).unwrap();
        be.bytes(&[0xAB; 32]).unwrap();
        be.bytes(&[0xCC; 8]).unwrap();
        be.bytes(&[0xDD; 16]).unwrap();

        let mut inner_buf = Vec::new();
        let mut ie = Encoder::new(&mut inner_buf);
        ie.array(2).unwrap();
        ie.u32(7).unwrap();
        ie.writer_mut().write_all(&block_buf).unwrap();

        let mut outer_buf = Vec::new();
        let mut oe = Encoder::new(&mut outer_buf);
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner_buf).unwrap();

        let body = BlockBody::new(outer_buf);
        let info = body.praos_inspect();
        assert_eq!(info.tx_count, 2);
        assert_eq!(info.field_count, 6);
        let cert = info.leios_cert.expect("cert shape should match");
        assert_eq!(cert.eb_slot, 12345);
        assert_eq!(cert.eb_hash, [0xAB; 32]);
    }

    #[test]
    fn praos_inspect_byron_returns_default() {
        let raw = build_test_block_with_tx_count(0, 0, None);
        let body = BlockBody::new(raw);
        let info = body.praos_inspect();
        // Byron path returns DecodeError → default
        assert_eq!(info.tx_count, 0);
        assert_eq!(info.field_count, 0);
    }

    #[test]
    fn praos_inspect_invalid_returns_default() {
        let body = BlockBody::opaque(vec![0xFF]);
        let info = body.praos_inspect();
        assert_eq!(info.tx_count, 0);
        assert_eq!(info.field_count, 0);
    }
}
