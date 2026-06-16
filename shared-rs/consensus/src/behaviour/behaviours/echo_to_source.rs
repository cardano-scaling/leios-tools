//! `EchoToSource` — adversarial behaviour that opens the LeiosNotify
//! no-echo gate.
//!
//! The honest policy in `serve_leios_notify` drops outbound
//! [`LeiosBlockOffer`](crate::behaviour::Outbound::LeiosBlockOffer) and
//! [`LeiosBlockTxsOffer`](crate::behaviour::Outbound::LeiosBlockTxsOffer)
//! entries whose `source == Some(peer)` — reflecting data back to the
//! peer that supplied it is a CIP-0164 violation.
//!
//! `EchoToSource` overrides
//! [`allow_echo_to_source`](crate::behaviour::Behaviour::allow_echo_to_source)
//! to `true`, so the I/O wrapper lets the echo through.  Combine with
//! [`LieAboutEbSize`](super::lie_about_eb_size::LieAboutEbSize) to
//! reproduce the duplex-follower bug that crashed earlier dev relays —
//! the size-zero offer reflected back to the source.

use crate::behaviour::{Behaviour, Outbound};
use crate::peer::PeerId;

/// Opens the no-echo gate on every outbound `LeiosNotify` send.  No
/// fields — composition with other behaviours is the only customisation
/// surface for now.
#[derive(Debug, Default)]
pub struct EchoToSource;

impl Behaviour for EchoToSource {
    fn name(&self) -> &'static str {
        "echo-to-source"
    }

    fn allow_echo_to_source(&mut self, _peer: PeerId, _out: &Outbound<'_>) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerId;
    use crate::types::Point;

    #[test]
    fn allows_echo_for_block_offer() {
        let mut behaviour = EchoToSource;
        let point = Point::Specific {
            slot: 100,
            hash: [0u8; 32],
        };
        let peer = PeerId(7);
        let out = Outbound::LeiosBlockOffer {
            point: &point,
            eb_size: 42,
            source: Some(peer),
        };
        assert!(behaviour.allow_echo_to_source(peer, &out));
    }

    #[test]
    fn allows_echo_for_block_txs_offer() {
        let mut behaviour = EchoToSource;
        let point = Point::Specific {
            slot: 200,
            hash: [1u8; 32],
        };
        let peer = PeerId(3);
        let out = Outbound::LeiosBlockTxsOffer {
            point: &point,
            source: Some(peer),
        };
        assert!(behaviour.allow_echo_to_source(peer, &out));
    }

    #[test]
    fn honest_default_blocks_echo() {
        // Sanity check the honest baseline.
        let mut honest = crate::behaviour::HonestBehaviour;
        let point = Point::Specific {
            slot: 100,
            hash: [0u8; 32],
        };
        let peer = PeerId(7);
        let out = Outbound::LeiosBlockOffer {
            point: &point,
            eb_size: 42,
            source: Some(peer),
        };
        assert!(!honest.allow_echo_to_source(peer, &out));
    }
}
