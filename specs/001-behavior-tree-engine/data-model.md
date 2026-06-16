# Phase 1 Data Model: Behavior Tree Engine

Types are placed in `shared-rs/consensus/src/behaviour/tree/`. They obey the
sans-IO/determinism rules of `shared-consensus` (no `tokio`, no clock reads,
`BTreeMap`/`BTreeSet` for ordered state, seeded RNG only). Field shapes follow the
structs the user supplied; names are aligned to the existing crate conventions.

> **Architecture**: this data model implements **Model B** — see
> [`design/unified-tick-model.md`](./design/unified-tick-model.md). The BT tick is the
> only place decisions are made; it emits a typed `Directives` value that mechanical
> actuators read at their (sometimes sub-tick) interception points. The old `Behaviour`
> hook trait, `BehaviourOutcome`/`DecisionOutcome`, and `CompositeBehaviour` are removed.

## Core status & seam types

### `NodeStatus`
The standard BT return value (spec FR-001).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus { Success, Failure, Running }
```

### `Directives` — the decision→actuation seam
Produced once per slot by `BehaviourTree::tick`; consumed by the consensus/I-O actuators
as a pure read. This is the typed contract that replaces the deleted hook trait.

**Indexed by actuator, not by behaviour, and domain-grouped.** The seam is keyed by the
shared consensus interception points (one vote decision, one production strategy, one
per-peer outbound transform, …), so multiple active leaves targeting the same resource
are reconciled in the tick (Model B invariant) and the actuator receives one resolved
value. It is grouped into per-domain sub-structs, each owned by its actuator domain, so
adding a behaviour that reuses an existing capability changes `Directives` **not at all**;
only a genuinely new *kind of effect* (a new actuator) extends it. See
[`design/unified-tick-model.md`](./design/unified-tick-model.md) §"The seam is owned by
actuators".

```rust
#[derive(Debug, Clone, Default)]
pub struct Directives {
    pub praos:   PraosDirectives,
    pub leios:   LeiosDirectives,
    pub mempool: MempoolDirectives,
}

#[derive(Debug, Clone, Default)]
pub struct PraosDirectives {
    pub production: RbProductionStrategy, // Normal | Suppress | Equivocate { ways }   (reused enum)
    pub outbound: OutboundDirective,      // None | EquivocateRouting { slot, ways, seed } | DropTo(BTreeSet<PeerId>)
    pub reorg_depth: Option<u64>,         // force self-reorg this slot if Some
    pub drop_inbound: bool,               // reset inbound peers this slot
    pub body_path: Option<BodyPath>,      // override producer body-path choice (reused enum)
}

#[derive(Debug, Clone, Default)]
pub struct LeiosDirectives {
    pub vote: VotePolicy,                 // Honest | Abstain(NoVoteReason)
}

#[derive(Debug, Clone, Default)]
pub struct MempoolDirectives {
    pub tx_filter: TxFilterPolicy,        // None | ChecksumThreshold { vote, non_voting, hide_eb_tx }
}

#[derive(Debug, Clone, Default)]
pub enum VotePolicy { #[default] Honest, Abstain(NoVoteReason) }

#[derive(Debug, Clone, Default)]
pub enum OutboundDirective {
    #[default] None,
    EquivocateRouting { slot: u64, ways: u8, seed: u64 }, // per-peer variant routing (lookup, not decision)
    DropTo(std::collections::BTreeSet<PeerId>),           // partition / mute
}
```

`Directives::default()` (every domain at its default) is the honest node (no
perturbation). `RbProductionStrategy`, `BodyPath`, and `NoVoteReason` are the **existing**
enums, reused unchanged.

**Reconciliation**: when two active leaves write the same field, the conflict is resolved
deterministically in the tick (e.g. last active contributor in deterministic traversal
order wins, or an explicit combine where it makes sense — mutually-exclusive cases like
`Suppress` vs. `Equivocate` are documented as a precedence). The actuator never combines.

**Publication**: shared-consensus only *computes* `Directives` (sans-IO). The net-node
wrapper applies it to the state machines once per slot (e.g.
`leios.apply_directives(&d)` sets the vote-policy/tx-filter fields) and publishes the
snapshot via a cheap shared cell (arc-swap / `tokio::sync::watch`) so the per-peer send
actuator in `net-core/server_handlers.rs` can read the latest without locking the loop.

## Environment & chain state

### `DynamicEnv` + `EnvHandle`
Externally mutable parameters (config + REST), read by conditions/actions (FR-010).

```rust
#[derive(Debug, Clone)]
pub struct DynamicEnv {
    pub trigger_slot: u64,            // Cardano slot to trigger on
    pub trigger_mempool_tx_count: u64 // mempool tx-count threshold to trigger on
    // extended as new strategies need parameters; every field is REST-addressable
}

pub type EnvHandle = std::sync::Arc<std::sync::RwLock<DynamicEnv>>;
```

`EnvHandle` mirrors the existing `BehaviourHandle = Arc<Mutex<…>>` pattern. The REST
handler takes the write lock briefly (never across an `.await`).

### `NativeChainState`
Read-only-to-the-tree node metrics, rebuilt each tick and passed by `&` (FR-011, D5).

```rust
#[derive(Debug, Clone, Default)]
pub struct NativeChainState {
    pub current_slot: u64,
    pub current_epoch: u64,
    pub mempool_tx_count: usize,
    // peers/etc. added as conditions require them (e.g. for contains(...))
}
```

## Tree structure

### `BehaviourTree`
The effective, validated tree, ticked as a unit.

```rust
pub struct BehaviourTree {
    name: String,
    revision: u32,
    seed: u64,
    root: NodeId,
    nodes: BTreeMap<NodeId, Node>,   // id -> node (ordered, deterministic)
    // per-node Running memory lives here or inside Node, see tick() contract
}

/// Everything a tick may read. Pure inputs — no I/O, no clock.
pub struct TickCtx<'a> {
    pub env: &'a DynamicEnv,          // read from EnvHandle by the caller before ticking
    pub state: &'a NativeChainState,  // rebuilt each tick, read-only
    pub seed: u64,                    // root seed for deterministic leaf choices
}

impl BehaviourTree {
    /// The ONLY place decisions happen. Evaluates conditions, resolves the active
    /// leaf set, accumulates each active leaf's contribution into one Directives.
    pub fn tick(&mut self, ctx: &TickCtx) -> (NodeStatus, Directives);
}
```

`NodeId` is the string `id` from TOML (newtype wrapper for clarity).

### `Node` and `NodeKind`

```rust
pub struct Node { pub id: NodeId, pub kind: NodeKind }

pub enum NodeKind {
    Selector { children: Vec<NodeId> },                 // first child to succeed
    Sequence { children: Vec<NodeId> },                 // all children in order
    Parallel { success_policy: SuccessPolicy, children: Vec<NodeId> },
    Condition { expr: ConditionExpr },                  // -> Success/Failure
    Action(ActionKind),                                 // leaf; contributes to Directives when active
}

pub enum SuccessPolicy { All, Any /* , N(usize) future */ }
```

**Composite semantics** (spec FR-003):
- `Selector`: tick children in order; first `Success` → `Success`; first `Running` →
  `Running` (remember index); all `Failure` → `Failure`.
- `Sequence`: tick children in order; first `Failure` → `Failure`; first `Running` →
  `Running` (remember index); all `Success` → `Success`.
- `Parallel`: tick all children; aggregate by `success_policy` (`All` = success iff all
  succeed; `Any` = success iff any succeeds); `Running` while undecided.

### `ActionKind` (leaf actions = directive contributors)
A leaf, when its branch is active this tick, contributes to the `Directives` accumulator
(it does **not** call into consensus). MVP set (confirmed scope Q1): honest + 1–2 real
demo actions. Leaf `kind`s are looked up via the retained registry (`build`).

```rust
pub enum ActionKind {
    /// Contributes nothing — leaves Directives at default (honest). Fallback branch.
    Honest,
    /// A re-homed catalogue behaviour, identified by its registry `kind` + params.
    /// `contribute()` writes the leaf's slice of Directives (e.g. the equivocator sets
    /// `out.praos.production = Equivocate{ways}` and `out.praos.outbound =
    /// EquivocateRouting{..}`).
    Behaviour(BehaviourSpec),
    // Future: NetworkShape { target, delay_ms, drop_rate }, TxGenerator { … }, …
}

// Each leaf contributes to the running Directives; no return-value flow control.
trait LeafAction {
    fn contribute(&mut self, ctx: &TickCtx, out: &mut Directives) -> NodeStatus;
}
```

`Behaviour(BehaviourSpec)` reuses `shared_consensus::behaviour::registry::BehaviourSpec`
as the **leaf-kind discriminant + parameter carrier**. Each shipped adversary
(`rb-header-equivocator`, `lazy-voter`, `t22`, `deep-reorg`, `drop-inbound-peers`) is
re-expressed as a `LeafAction` whose `contribute` sets the matching `Directives` fields.
This is the "keep registration, re-home mechanics" decision (D2 / Model B). Composition
that used to be `BehaviourSpec::Composite` is now expressed by the BT structure itself
(`Parallel`/`Sequence`).

### `ConditionExpr`
Minimal grammar (D6); parsed and validated at load time.

```rust
pub enum ConditionExpr {
    Compare { lhs: ValueRef, op: CompareOp, rhs: ValueRef },
    Contains { container: ValueRef, item: ValueRef },
    And(Vec<ConditionExpr>),
    Or(Vec<ConditionExpr>),
    Not(Box<ConditionExpr>),
}
pub enum CompareOp { Ge, Gt, Le, Lt, Eq, Ne }
pub enum ValueRef { Env(String), Chain(String), LitU64(u64), LitStr(String) }
```

Validation: every `Env(name)` must exist in `[env]`; every `Chain(name)` must be a
known `NativeChainState` field; type mismatches (string vs. u64) are load-time errors.

## Configuration (TOML → types)

### `BtConfig`
The on-disk form (matches the spec's example; see `contracts/bt-config.schema.md`).

```rust
pub struct BtConfig {
    pub metadata: Metadata,          // name, revision, seed
    pub env: BTreeMap<String, EnvValue>,
    pub nodes: Vec<RawNode>,         // [[nodes]] entries
    pub includes: Vec<String>,       // relative paths to sub-behaviour TOMLs
}
pub struct Metadata { pub name: String, pub revision: u32, pub seed: u64 }
```

- `BtConfig::load(path)` resolves `includes` relative to `path`, detects cycles, merges
  (root overrides include for same-id nodes / same env key), then `validate()`s and
  compiles to `BehaviourTree`.
- `EnvValue` is a small typed union (`U64`/`F64`/`Str`/`Bool`) so conditions can
  type-check references.

## Validation rules (spec FR-013) — all enforced at load, before activation

1. Exactly one root; `root` (or first node) must resolve to a defined node.
2. Every `children` / include reference resolves to a defined node / readable file.
3. No cycles in the node graph or the include graph.
4. Every node `type` is known; `Parallel` requires a valid `success_policy`.
5. Every `Condition` references only declared `[env]` keys and known chain-state fields,
   with matching types.
6. Every `Action` resolves to a known leaf via the registry (e.g. a `Behaviour` action's
   `spec` deserialises into a known `BehaviourSpec` `kind`) and its `contribute` maps to
   representable `Directives` fields.
7. `[metadata]` has a `seed` (reproducibility, FR-009).

Failures return a precise `Result::Err` with the offending id/path/field; the node
refuses to start (US1 scenario 5) or the REST replace is rejected while the prior tree
stays active (US2 scenario 3/4).

## Relationships

```text
BtConfig --load/validate--> BehaviourTree
BehaviourTree.nodes[id] = Node{ kind }
NodeKind::Action(Behaviour(spec)) --registry build(kind)--> LeafAction
tick(ctx) --accumulates contribute()--> Directives
net-node main loop --apply_directives--> leios/praos/mempool state (vote policy, tx filter, …)
net-node main loop --publish--> Directives snapshot (arc-swap/watch)
   --read by--> production.rs (production/body_path) + server_handlers.rs (outbound) + main.rs (reorg/drop)
EnvHandle <--writes-- net-node REST handler (DEFERRED / post-MVP, Docker)
NativeChainState <--built each tick from-- slot_clock + mempool (net-node main loop)
```

## State transitions (per node, across ticks)

- A composite holds at most one "running child index"; cleared when the node next
  resolves to `Success`/`Failure`.
- The whole-tree effect is the `Directives` produced this tick. Because the actuators
  are pure reads of the latest `Directives`, a leaf that stops being active simply stops
  contributing, and the next tick's `Directives` reverts those fields to default — there
  is no separate "deactivate" call to make. Honest = `Directives::default()`.
- Directives are recomputed every tick, so there is no stale activation to diff; the
  published snapshot is replaced wholesale each slot.
