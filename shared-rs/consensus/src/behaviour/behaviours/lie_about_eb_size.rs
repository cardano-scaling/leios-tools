//! `LieAboutEbSize` — adversarial behaviour that overrides the
//! `eb_size` field on outbound `MsgLeiosBlockOffer`.
//!
//! CIP-0164 requires `eb_size` to match the encoded EB byte length so
//! the receiving peer can pre-size its fetch buffer.  Earlier dev
//! relays crashed when sent `eb_size = 0`.  This behaviour replaces
//! the honest value with `(eb_size * scale_num / scale_den) + offset`,
//! clamped to `u32`.
//!
//! - Identity (no-op): `scale_num = 1, scale_den = 1, offset = 0`.
//! - The original bug shape (size-zero): `scale_num = 0, scale_den = 1,
//!   offset = 0`.
//! - "Off by one" (test relay rounding tolerance):
//!   `scale_num = 1, scale_den = 1, offset = 1`.
//! - "Always understate by half": `scale_num = 1, scale_den = 2,
//!   offset = 0`.
//!
//! The transform leaves [`Outbound::LeiosBlockTxsOffer`] untouched
//! (that variant carries no `eb_size`) and ignores
//! [`Outbound::RbHeader`].

use crate::behaviour::{Behaviour, Outbound, OutboundDecision, OwnedOutbound};
use crate::peer::PeerId;

/// Mutates `eb_size` on outbound `MsgLeiosBlockOffer` via the linear
/// transform `(eb_size * scale_num / scale_den) + offset`.  `scale_den`
/// of `0` is treated as `1` to avoid panics.
#[derive(Debug, Clone)]
pub struct LieAboutEbSize {
    /// Numerator of the size-scaling fraction.
    pub scale_num: u32,
    /// Denominator of the size-scaling fraction.  Clamped to `>= 1` at
    /// every call; storing `0` is allowed but interpreted as `1`.
    pub scale_den: u32,
    /// Additive offset applied after scaling.  Final size is clamped to
    /// `[0, u32::MAX]`.
    pub offset: i32,
}

impl LieAboutEbSize {
    /// Constructor.  `scale_den` of `0` is clamped to `1`.
    pub fn new(scale_num: u32, scale_den: u32, offset: i32) -> Self {
        Self {
            scale_num,
            scale_den: scale_den.max(1),
            offset,
        }
    }

    /// The original bug shape: `eb_size = 0` regardless of the real EB
    /// byte length.  Equivalent to `LieAboutEbSize::new(0, 1, 0)`.
    pub fn zero() -> Self {
        Self::new(0, 1, 0)
    }

    /// Compute the mutated size for a given honest `eb_size`.  i128
    /// intermediates keep the arithmetic well-defined across the whole
    /// u32 input range and any i32 offset; the result is clamped to
    /// `[0, u32::MAX]`.
    fn mutate_size(&self, eb_size: u32) -> u32 {
        let den = self.scale_den.max(1) as i128;
        let scaled = (eb_size as i128) * (self.scale_num as i128) / den;
        let with_offset = scaled + (self.offset as i128);
        with_offset.clamp(0, u32::MAX as i128) as u32
    }
}

impl Behaviour for LieAboutEbSize {
    fn name(&self) -> &'static str {
        "lie-about-eb-size"
    }

    fn transform_outbound(&mut self, _peer: PeerId, out: Outbound<'_>) -> OutboundDecision {
        match out {
            Outbound::LeiosBlockOffer {
                point,
                eb_size,
                source,
            } => {
                let new_size = self.mutate_size(eb_size);
                OutboundDecision::Replace(OwnedOutbound::LeiosBlockOffer {
                    point: point.clone(),
                    eb_size: new_size,
                    source,
                })
            }
            _ => OutboundDecision::Send,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerId;
    use crate::types::Point;

    #[test]
    fn zero_constructor_yields_zero_size() {
        let b = LieAboutEbSize::zero();
        assert_eq!(b.mutate_size(0), 0);
        assert_eq!(b.mutate_size(42), 0);
        assert_eq!(b.mutate_size(u32::MAX), 0);
    }

    #[test]
    fn identity_no_op() {
        let b = LieAboutEbSize::new(1, 1, 0);
        for eb_size in [0u32, 1, 42, 78627, u32::MAX] {
            assert_eq!(b.mutate_size(eb_size), eb_size, "eb_size={eb_size}");
        }
    }

    #[test]
    fn off_by_constant() {
        let b = LieAboutEbSize::new(1, 1, 1);
        assert_eq!(b.mutate_size(0), 1);
        assert_eq!(b.mutate_size(42), 43);
        let b_neg = LieAboutEbSize::new(1, 1, -1);
        assert_eq!(b_neg.mutate_size(43), 42);
    }

    #[test]
    fn halving() {
        let b = LieAboutEbSize::new(1, 2, 0);
        assert_eq!(b.mutate_size(100), 50);
        assert_eq!(b.mutate_size(101), 50);
        assert_eq!(b.mutate_size(0), 0);
    }

    #[test]
    fn doubling() {
        let b = LieAboutEbSize::new(2, 1, 0);
        assert_eq!(b.mutate_size(100), 200);
        assert_eq!(b.mutate_size(u32::MAX / 2), u32::MAX - 1);
    }

    #[test]
    fn clamp_below_zero() {
        // Offset wants -10, scaled value is 5 → final -5 → clamped to 0.
        let b = LieAboutEbSize::new(1, 1, -10);
        assert_eq!(b.mutate_size(5), 0);
    }

    #[test]
    fn clamp_above_u32_max() {
        // 2*u32::MAX overflows u32 — should clamp.
        let b = LieAboutEbSize::new(2, 1, 0);
        assert_eq!(b.mutate_size(u32::MAX), u32::MAX);
    }

    #[test]
    fn zero_denominator_treated_as_one() {
        let b = LieAboutEbSize::new(3, 0, 0);
        // 42 * 3 / 1 = 126
        assert_eq!(b.mutate_size(42), 126);
    }

    #[test]
    fn transform_replaces_block_offer() {
        let mut b = LieAboutEbSize::zero();
        let point = Point::Specific {
            slot: 100,
            hash: [7u8; 32],
        };
        let source = Some(PeerId(3));
        let out = Outbound::LeiosBlockOffer {
            point: &point,
            eb_size: 78627,
            source,
        };
        match b.transform_outbound(PeerId(1), out) {
            OutboundDecision::Replace(OwnedOutbound::LeiosBlockOffer {
                point: p,
                eb_size,
                source: s,
            }) => {
                assert_eq!(p, point);
                assert_eq!(eb_size, 0);
                assert_eq!(s, source);
            }
            other => panic!("expected Replace(LeiosBlockOffer), got {other:?}"),
        }
    }

    #[test]
    fn transform_leaves_other_artefacts_alone() {
        let mut b = LieAboutEbSize::zero();
        let point = Point::Specific {
            slot: 200,
            hash: [9u8; 32],
        };
        // BlockTxsOffer: no eb_size to mutate.
        let out = Outbound::LeiosBlockTxsOffer {
            point: &point,
            source: Some(PeerId(5)),
        };
        assert!(matches!(
            b.transform_outbound(PeerId(1), out),
            OutboundDecision::Send
        ));
        // RbHeader: not our concern.
        let header = vec![0u8; 32];
        let out = Outbound::RbHeader {
            slot: 100,
            header: &header,
        };
        assert!(matches!(
            b.transform_outbound(PeerId(1), out),
            OutboundDecision::Send
        ));
    }
}
