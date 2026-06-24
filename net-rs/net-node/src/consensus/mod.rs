//! Consensus facade.
//!
//! Owns both the Praos (longest-chain RB) and Leios (EB/vote) sub-layers and
//! dispatches incoming network events to whichever cares. Praos is the
//! foundation; Leios sits on top and produces votes when EB elections enter
//! the Voting pipeline phase.

mod leios;
mod praos;

use net_core::multi_peer::types::{NetworkCommand, NetworkEvent};
use net_core::types::{BlockBody, Point, Tip, WrappedHeader};
use tokio::sync::{mpsc, watch};

use crate::config::{CommitteeSelection, DynamicConfig, FetchPolicyConfig, StakeEntry};
use crate::telemetry::NodeEvent;
use crate::validation::{LedgerOutcome, Validator};
use shared_consensus::chain_tree::ChainTreeEntry;
use shared_consensus::fetch::PeerRttCache;
use shared_consensus::leios::ChainTipContext;

pub use leios::{EbTxMatchOutcome, LeiosConsensus, PipelineConfig};
pub use praos::PraosConsensus;
use shared_consensus::mempool::TxBody;

/// Top-level consensus, composing Praos and Leios sub-layers.
pub struct Consensus {
    praos: PraosConsensus,
    leios: LeiosConsensus,
}

impl Consensus {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: String,
        commands: mpsc::Sender<NetworkCommand>,
        validator: Validator,
        mempool: crate::mempool::SharedMempool,
        security_param_k: u64,
        pipeline: PipelineConfig,
        committee_selection: CommitteeSelection,
        stake: u64,
        stake_registry: &[StakeEntry],
        persistent_vote_bytes: usize,
        non_persistent_vote_bytes: usize,
        quorum_weight_fraction: f64,
        committee_seed: u64,
        rng_seed: Option<u64>,
        dyn_config: watch::Receiver<DynamicConfig>,
        rtt: PeerRttCache,
        fetch_policy: FetchPolicyConfig,
    ) -> Self {
        let mut praos = PraosConsensus::new(
            node_id.clone(),
            commands.clone(),
            validator.clone(),
            security_param_k,
        );
        praos.set_rtt(rtt.clone());
        praos.set_block_policy(fetch_policy.block.into_block_policy());
        let mut leios = LeiosConsensus::new(
            node_id,
            commands,
            validator,
            mempool.clone(),
            pipeline,
            committee_selection,
            stake,
            stake_registry,
            persistent_vote_bytes,
            non_persistent_vote_bytes,
            quorum_weight_fraction,
            committee_seed,
            rng_seed,
            dyn_config,
        );
        leios.set_rtt(rtt);
        leios.set_eb_policy(fetch_policy.eb.into_eb_policy());
        leios.set_eb_txs_policy(fetch_policy.eb_txs.into_eb_txs_policy());
        Self { praos, leios }
    }

    /// Apply the per-slot behaviour-tree [`ControlSignal`] to the consensus
    /// state machines. Called once per slot by the I/O loop after ticking the
    /// tree. The Leios state reads its vote policy and the t22 EB-processing
    /// filter from this; the Praos state holds it for the production/reorg/drop
    /// actuators the I/O loop consults.
    ///
    /// [`ControlSignal`]: shared_consensus::behaviour::tree::ControlSignal
    pub fn apply_control(&mut self, control: &shared_consensus::behaviour::tree::ControlSignal) {
        self.leios.state_mut().apply_control(control);
        self.praos.state_mut().apply_control(control);
    }

    /// Force a deliberate self-reorg of `depth` blocks (the `deep-reorg`
    /// action, via `control.praos.reorg_depth`): roll the adopted chain back
    /// and diffuse the rollback to peers. Returns `true` if a rollback actually
    /// happened (the chain was long enough). The decision lives in the BT tick;
    /// this is the mechanical actuator.
    pub async fn force_reorg(&mut self, depth: u64) -> bool {
        self.praos.force_rollback(depth).await
    }

    /// Notify the Leios layer of a new slot tick.
    pub async fn on_slot(&mut self, slot: u64) {
        // Bump Praos's slot first so subsequent header-arrival paths
        // (TipAdvanced, BlockReceived, register_self_produced) stamp
        // the right slot on `note_header_first_seen`.  Then refresh the
        // chain-tip context Leios uses for the CIP-0164 voting
        // predicates before driving elections forward.
        self.praos.set_current_slot(slot);
        self.refresh_chain_tip_ctx();
        self.leios.on_slot(slot).await;
    }

    fn refresh_chain_tip_ctx(&mut self) {
        let arrival = self.praos.adopted_tip_header_arrival_slot();
        let eb_announcement = self.praos.adopted_tip_announced_eb();
        let equivocating_slots = self.praos.equivocating_rb_slots().clone();
        let tip_rb_slot = self.praos.state().adopted_tip_rb_slot();
        self.leios.set_chain_tip_context(ChainTipContext {
            rb_header_arrival_slot: arrival,
            eb_announcement,
            equivocating_slots,
            tip_rb_slot,
        });
    }

    /// Register a self-produced ranking block with Praos consensus.
    pub async fn register_self_produced(
        &mut self,
        point: &Point,
        header: &WrappedHeader,
        body: &BlockBody,
    ) {
        self.praos.register_self_produced(point, header, body).await
    }

    /// Register a self-produced endorser block with Leios consensus —
    /// records the manifest, fires the offer notifications, and marks
    /// the EB validated.  Also stashes the manifest size on the
    /// announcing RB's chain-tree node so the UI snapshot can surface
    /// the count regardless of LeiosState's manifest-cache TTL.
    pub async fn register_self_produced_eb(&mut self, point: Point, eb_data: &[u8]) {
        self.record_announced_eb_tx_count_from_blob(&point, eb_data);
        self.leios.register_self_produced_eb(point, eb_data).await;
    }

    /// Route a network event to Praos or Leios. Returns true if the event
    /// was consumed (caller should not log it separately).
    pub async fn handle_event(&mut self, event: &NetworkEvent) -> bool {
        // Mirror manifest sizes onto the chain-tree node on receive so
        // they survive the LeiosState cache TTL — see
        // `register_self_produced_eb` for the same on the produce path.
        if let NetworkEvent::LeiosBlockReceived { point, block, .. } = event {
            self.record_announced_eb_tx_count_from_blob(point, block);
        }
        match event {
            NetworkEvent::LeiosBlockOffered { .. }
            | NetworkEvent::LeiosBlockTxsOffered { .. }
            | NetworkEvent::LeiosBlockReceived { .. }
            | NetworkEvent::LeiosVotesReceived { .. }
            | NetworkEvent::LeiosBlockTxsReceived { .. } => self.leios.handle_event(event).await,
            _ => self.praos.handle_event(event).await,
        }
    }

    /// Decode an EB blob to extract its manifest size and stash it on
    /// the chain-tree node that announced this EB hash.  Idempotent;
    /// no-op when the blob doesn't decode (malformed) or no chain-tree
    /// node announces the EB (the announcing RB was pruned or never
    /// adopted).
    fn record_announced_eb_tx_count_from_blob(&mut self, point: &Point, blob: &[u8]) {
        let hash = match point {
            Point::Specific { hash, .. } => *hash,
            Point::Origin => return,
        };
        if let Some(tx_hashes) = net_codec::decode_overflow_eb(blob) {
            self.praos
                .record_announced_eb_tx_count(&hash, tx_hashes.len() as u32);
        }
    }

    /// Verify a `LeiosBlockTxsReceived` response against the cached
    /// EB manifest. Returns the bodies whose hash lies in the manifest,
    /// in manifest-index order, plus how many indices were requested
    /// and which indices remain unfilled.
    pub fn match_eb_tx_response(&mut self, point: &Point, bodies: &[TxBody]) -> EbTxMatchOutcome {
        self.leios.match_eb_tx_response(point, bodies)
    }

    /// Re-issue a `FetchLeiosBlockTxs` for the still-missing indices.
    /// The coordinator's `pick_txs_fetch_peer` excludes already-tried
    /// peers, so the retry will land on a different candidate (or no-op
    /// if all candidates are exhausted).
    pub async fn retry_eb_tx_fetch(
        &mut self,
        point: Point,
        bitmap: std::collections::BTreeMap<u16, u64>,
    ) {
        self.leios.retry_eb_tx_fetch(point, bitmap).await;
    }

    /// Periodic retry for lagging nodes — evicts stale fetches and
    /// re-runs chain selection even when no network events arrive.
    pub async fn retry_pending(&mut self) {
        self.praos.retry_select_chain().await;
    }

    pub async fn on_validation_outcome(&mut self, outcome: LedgerOutcome) -> bool {
        match outcome {
            LedgerOutcome::EbValidated { point } => {
                self.leios.on_validated_eb(point);
                false
            }
            LedgerOutcome::Applied { ref point } => {
                // Producer-side EB-safety gate: an RB carrying a cert
                // for the parent's announced EB needs that EB recorded
                // in `LeiosState` until its body validates locally.
                // `BodyPath::decide` reads this for the next own RB.
                if let Some((eb_slot, eb_hash)) = self.praos.parent_announced_eb_for_cert(point) {
                    // Permanent diagnostic: surfaces every chain-committed
                    // certification we observe.  Fires sparsely (only
                    // RBs whose header set `certified_eb=true` AND whose
                    // parent carried an `announced_eb_hash`), so the cost
                    // is one INFO line per real cert.  Cross-references
                    // the `eb_announced` field on the prior block's
                    // "block received and cached" log.
                    tracing::info!(
                        node_id = %self.praos.node_id_str(),
                        %point,
                        eb_slot,
                        eb_hash = %eb_hash.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "RB applied with cert for parent's announced EB"
                    );
                    self.leios.state.on_chain_endorsement(eb_slot, eb_hash);
                }
                self.praos.on_validation_outcome(outcome).await
            }
            other => self.praos.on_validation_outcome(other).await,
        }
    }

    #[allow(dead_code)]
    pub fn local_tip(&self) -> Option<Tip> {
        self.praos.local_tip()
    }

    pub fn chain_tree_snapshot(&self) -> (Vec<ChainTreeEntry>, Option<u64>, Option<String>) {
        let leios_state = &self.leios.state;
        self.praos.chain_tree_snapshot(|eb_hash| {
            leios_state
                .eb_tx_hashes
                .get(eb_hash)
                .map(|(_, hashes)| hashes.len() as u32)
        })
    }

    pub fn tip_hash(&self) -> Option<[u8; 32]> {
        self.praos.tip_hash()
    }

    pub fn next_block_number(&self) -> u64 {
        self.praos.next_block_number()
    }

    /// Borrow the underlying `LeiosState`.  Used by `try_produce_block`
    /// to consult the producer-side EB-safety gate
    /// (`BodyPath::decide` reads `has_endorsed_unvalidated_eb`).
    pub fn leios_state(&self) -> &shared_consensus::leios::LeiosState {
        &self.leios.state
    }

    /// Linear-Leios producer rule: an RB may attach a cert only for the
    /// EB its **parent RB** announced, and only once that EB has reached
    /// quorum and entered CertEligible.  Returns the announced slot of
    /// that EB (to populate the `RbCertifiedEb` telemetry event); `None`
    /// means no cert candidate — the producer leaves `certified_eb` off
    /// and the parent's EB is dropped from the chain's perspective.
    pub fn cert_for_parent(&self) -> Option<u64> {
        let eb_hash = self.praos.adopted_tip_announced_eb()?;
        self.leios.eb_certifiable_slot(&eb_hash)
    }

    /// Emit per-subsystem `info!` lines summarising internal state
    /// collection sizes.  Used as a periodic diagnostic to identify
    /// unbounded growth — grep `state sizes` in node logs to read the
    /// time series.
    pub fn log_state_sizes(&self) {
        self.praos.state().log_state_sizes();
        self.leios.state.log_state_sizes();
    }

    /// Snapshot Praos state collection sizes (plus byte estimates for the
    /// equivocation maps).  Used by the per-slot memory telemetry path.
    pub fn praos_state_sizes(&self) -> shared_consensus::praos::PraosStateSizes {
        self.praos.state().state_sizes()
    }

    /// Snapshot Leios state collection sizes.  Used by the per-slot
    /// memory telemetry path.
    pub fn leios_state_sizes(&self) -> shared_consensus::leios::LeiosStateSizes {
        self.leios.state.state_sizes()
    }

    /// Drain Leios-side telemetry events buffered since the last call.
    pub fn drain_leios_telemetry(&mut self) -> Vec<NodeEvent> {
        self.leios.drain_telemetry()
    }
}
