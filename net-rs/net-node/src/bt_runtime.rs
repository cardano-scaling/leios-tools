//! Behaviour-tree runtime for the node.
//!
//! Holds the compiled [`BehaviourTree`] and its resolved env, and ticks the
//! tree once per slot to produce the slot's [`ControlSignal`]. The I/O loop
//! applies that signal to the consensus state machines (`Consensus::apply_control`).
//!
//! The config is **self-contained** (includes already resolved by
//! `bt.py --resolve`); `BtConfig::parse`/`compile` reject an unresolved config.
//! Loading reads the file here (the engine itself is sans-IO).

use std::sync::{Arc, Mutex};

use shared_consensus::behaviour::tree::actions::HonestAction;
use shared_consensus::behaviour::tree::{
    Behaviour, BehaviourKind, BehaviourTree, BtConfig, ControlSignal, DynamicEnv,
    EquivocationVariants, NativeChainState, TickCtx,
};
use tokio::sync::watch;

/// Shared, slot-keyed store of RB-header equivocation variants. The producer
/// (net-node) writes the variants it builds on an equivocating slot; the
/// per-peer send actuators (net-core `server_handlers`) read them.
pub type SharedVariants = Arc<Mutex<EquivocationVariants>>;

/// The node's behaviour-tree runtime.
#[derive(Debug)]
pub struct BtRuntime {
    /// Resolved env the conditions read (from the config's `[env]`).
    env: DynamicEnv,
    /// The compiled tree, ticked once per slot.
    tree: BehaviourTree,
    /// Publishes the latest per-slot control signal to the per-peer outbound
    /// actuators (which hold `watch::Receiver` clones).
    control_tx: watch::Sender<ControlSignal>,
    /// Equivocation variants produced this run, shared with the send actuators.
    variants: SharedVariants,
}

impl BtRuntime {
    /// An implicit honest runtime: a single honest leaf, empty env. Used when
    /// no `--behaviour-tree` is supplied.
    pub fn honest() -> Self {
        let root = Behaviour::new("honest", BehaviourKind::Action(Box::new(HonestAction)));
        Self::from_parts(DynamicEnv::new(), BehaviourTree::new("honest", 0, root))
    }

    fn from_parts(env: DynamicEnv, tree: BehaviourTree) -> Self {
        let (control_tx, _rx) = watch::channel(ControlSignal::default());
        BtRuntime {
            env,
            tree,
            control_tx,
            variants: Arc::new(Mutex::new(EquivocationVariants::new())),
        }
    }

    /// Subscribe to the published per-slot control signal (for the per-peer
    /// send actuators). The latest value is always available via `borrow()`.
    pub fn subscribe(&self) -> watch::Receiver<ControlSignal> {
        self.control_tx.subscribe()
    }

    /// A handle to the shared equivocation-variant store.
    pub fn variants(&self) -> SharedVariants {
        self.variants.clone()
    }

    /// Build a runtime from a self-contained BT config (TOML text). Returns a
    /// human-readable error on parse/validation failure so the node can refuse
    /// to start (US1 scenario 5).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let cfg = BtConfig::parse(text).map_err(|e| e.to_string())?;
        let env = DynamicEnv(cfg.env.clone());
        let tree = cfg.compile().map_err(|e| e.to_string())?;
        Ok(Self::from_parts(env, tree))
    }

    /// Load and compile a BT config from a file path.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
        Self::from_toml(&text)
    }

    /// The run name (for telemetry / startup logging).
    pub fn name(&self) -> &str {
        self.tree.name()
    }

    /// The reproducibility seed.
    pub fn seed(&self) -> u64 {
        self.tree.seed()
    }

    /// Tick the tree for `slot`, returning the slot's control signal. The
    /// caller applies it to the consensus state machines.
    pub fn tick(&mut self, slot: u64, epoch: u64, mempool_tx_count: usize) -> ControlSignal {
        let state = NativeChainState {
            current_slot: slot,
            current_epoch: epoch,
            mempool_tx_count,
        };
        let seed = self.tree.seed();
        let ctx = TickCtx {
            env: &self.env,
            state: &state,
            seed,
        };
        let (_status, control) = self.tree.tick(&ctx);
        // Publish for the per-peer send actuators. Ignore the no-receiver case
        // (no peers subscribed yet); the latest value is retained regardless.
        let _ = self.control_tx.send(control.clone());
        control
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_consensus::behaviour::tree::control::VotePolicy;

    #[test]
    fn honest_runtime_emits_default_control() {
        let mut rt = BtRuntime::honest();
        assert_eq!(rt.name(), "honest");
        let control = rt.tick(10, 0, 0);
        assert_eq!(control, ControlSignal::default());
    }

    #[test]
    fn slot_trigger_config_switches_to_adversarial() {
        let cfg = r#"
[run]
name = "lazy-after-trigger"
seed = 1
root = "root"
[env]
trigger_slot = 50
[behaviours.root]
type = "Selector"
children = ["attack", "honest"]
[behaviours.attack]
type = "Sequence"
children = ["cond", "lazy"]
[behaviours.cond]
type = "Condition"
expression = "cardano.current_slot >= env.trigger_slot"
[behaviours.lazy]
type = "Action"
spec = { kind = "lazy-voter" }
[behaviours.honest]
type = "HonestAction"
"#;
        let mut rt = BtRuntime::from_toml(cfg).unwrap();
        // Before the trigger: honest.
        assert_eq!(rt.tick(49, 0, 0).leios.vote, VotePolicy::Honest);
        // At/after: lazy-voter abstains.
        assert!(matches!(
            rt.tick(50, 0, 0).leios.vote,
            VotePolicy::Abstain(_)
        ));
    }

    #[test]
    fn unresolved_includes_are_rejected() {
        let cfg = r#"
includes = ["x.bt"]
[run]
name = "x"
seed = 1
root = "honest"
[behaviours.honest]
type = "HonestAction"
"#;
        let err = BtRuntime::from_toml(cfg).unwrap_err();
        assert!(err.contains("bt.py --resolve"), "{err}");
    }
}
