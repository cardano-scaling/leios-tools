# Phase 1 Data Model: Behavior Tree Engine

Types live in `shared-rs/consensus/src/behaviour/tree/` and obey the crate's
sans-IO/determinism rules (no `tokio`, no clock reads, `BTreeMap`/`BTreeSet` for ordered
state, seeded RNG only). Architecture and rationale: [`design/unified-tick-model.md`](./design/unified-tick-model.md)
and research D2/D10/D13.

## Core status & seam types

### `Status`
The BT return value (spec FR-001).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status { Success, Failure, Running }
```

### `ControlSignal` — the decision→actuation seam
Produced once per slot by `BehaviourTree::tick`, read by the actuators. Domain-grouped by
actuator (`praos`/`leios`/`mempool`); each active leaf writes its slice; same-field
conflicts are reconciled in the tick.

```rust
#[derive(Debug, Clone, Default)]
pub struct ControlSignal {
    pub praos:   PraosControl,
    pub leios:   LeiosControl,
    pub mempool: MempoolControl,
}

#[derive(Debug, Clone, Default)]
pub struct PraosControl {
    pub production: RbProductionStrategy, // Normal | Suppress | Equivocate { ways }   (reused enum)
    pub outbound: OutboundControl,      // None | EquivocateRouting { slot, ways, seed } | DropTo(BTreeSet<PeerId>)
    pub reorg_depth: Option<u64>,         // force self-reorg this slot if Some
    pub drop_inbound: bool,               // reset inbound peers this slot
    pub body_path: Option<BodyPath>,      // override producer body-path choice (reused enum)
}

#[derive(Debug, Clone, Default)]
pub struct LeiosControl {
    pub vote: VotePolicy,                 // Honest | Abstain(NoVoteReason)
}

#[derive(Debug, Clone, Default)]
pub struct MempoolControl {
    pub tx_filter: TxFilterPolicy,        // None | ChecksumThreshold { vote, non_voting, hide_eb_tx }
}

#[derive(Debug, Clone, Default)]
pub enum VotePolicy { #[default] Honest, Abstain(NoVoteReason) }

#[derive(Debug, Clone, Default)]
pub enum OutboundControl {
    #[default] None,
    EquivocateRouting { slot: u64, ways: u8, seed: u64 }, // per-peer variant routing (lookup, not decision)
    DropTo(std::collections::BTreeSet<PeerId>),           // partition / mute
}
```

`ControlSignal::default()` is the honest node (no perturbation). `RbProductionStrategy`,
`BodyPath`, and `NoVoteReason` are existing enums, reused unchanged.

- **Conflicts**: same-field writes from two active leaves are reconciled in the tick
  (last active contributor in traversal order wins); the actuator never combines.
- **Publication**: shared-consensus only *computes* `ControlSignal` (sans-IO); the net-node
  wrapper applies it to the state machines each slot (`leios.apply_control(&d)`, …) and
  publishes the snapshot (arc-swap / `tokio::watch`) for the per-peer send actuator.

## Environment & chain state

### `DynamicEnv` + `EnvHandle`
Externally mutable parameters (config + REST), read by conditions/actions (FR-010).

`DynamicEnv` is the **resolved env**: a name-keyed map of typed values (not a fixed struct,
so arbitrary params can be declared in TOML, overlaid across includes, and addressed by REST
`:key`). Keys may be dotted for owner-namespaced params (`network_shape.packet_delay`) vs.
shared (`trigger_slot`) — see `contracts/bt-config.schema.md`.

```rust
#[derive(Debug, Clone, Default)]
pub struct DynamicEnv(pub std::collections::BTreeMap<String, EnvValue>);

#[derive(Debug, Clone, PartialEq)]
pub enum EnvValue { U64(u64), F64(f64), Str(String), Bool(bool) }

pub type EnvHandle = std::sync::Arc<std::sync::RwLock<DynamicEnv>>;
```

A (deferred) REST handler takes the `EnvHandle` write lock briefly to overwrite one key;
a type-mismatched write is rejected, leaving the env unchanged.

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
    name: String,                    // from [run].name
    seed: u64,                       // from [run].seed (the one reproducibility seed)
    root: BehaviourId,                    // from [run].root
    behaviours: BTreeMap<BehaviourId, Behaviour>,  // id -> behaviour (ordered, deterministic; ids may be dotted)
    // per-behaviour Running memory lives here or inside Behaviour, see tick() contract
    // (per-module `revision` lives in module metadata, not at tree level)
}

/// Everything a tick may read. Pure inputs — no I/O, no clock.
pub struct TickCtx<'a> {
    pub env: &'a DynamicEnv,          // read from EnvHandle by the caller before ticking
    pub state: &'a NativeChainState,  // rebuilt each tick, read-only
    pub seed: u64,                    // root seed for deterministic leaf choices
}

impl BehaviourTree {
    /// The ONLY place decisions happen. Evaluates conditions, resolves the active
    /// leaf set, accumulates each active leaf's contribution into one ControlSignal.
    pub fn tick(&mut self, ctx: &TickCtx) -> (Status, ControlSignal);
}
```

`BehaviourId` is the string `id` from TOML (newtype wrapper for clarity).

### `Behaviour` and `BehaviourKind`

```rust
pub struct Behaviour { pub id: BehaviourId, pub kind: BehaviourKind }

pub enum BehaviourKind {
    Selector { children: Vec<BehaviourId> },   // ordered OR  (first to succeed)
    Sequence { children: Vec<BehaviourId> },   // ordered AND (stop on first failure)
    Join     { children: Vec<BehaviourId> },   // concurrent AND, fail-fast (no policy field)
    Condition { expr: ConditionExpr },         // -> Success/Failure (immediate)
    Action(ActionKind),                        // leaf; contributes to ControlSignal when active
}
```

**Composite semantics** — **reactive** (re-evaluate from the first child every tick); full
operational semantics + the `halt`/abort relation are in
[`design/bt-grammar-and-semantics.md`](./design/bt-grammar-and-semantics.md):
- `Sequence` (ordered AND): tick from the start; first `Failure` → `Failure`; first
  `Running` → `Running`; all `Success` → `Success`. Children after the deciding one are
  **halted**. A `Condition` precondition that flips to `Failure` thus aborts a running
  later child.
- `Selector` (ordered OR): tick from the start; first `Success` → `Success`; first
  `Running` → `Running`; all `Failure` → `Failure`. Children after the deciding one are
  halted.
- `Join` (concurrent AND, fail-fast): tick every not-yet-succeeded child each tick;
  **any** `Failure` halts all children and → `Failure`; **all** `Success` → `Success`;
  otherwise `Running`. Policy is fixed (all-succeed); there is no `success_policy` field.

### `ActionKind` (leaf actions = control-signal contributors)
A leaf, when its branch is active this tick, contributes to the `ControlSignal` accumulator
(it does **not** call into consensus). MVP set (confirmed scope Q1): honest + 1–2 real
demo actions. Leaf `kind`s are looked up via the retained registry (`build`).

```rust
pub enum ActionKind {
    /// Contributes nothing — leaves ControlSignal at default (honest). Fallback branch.
    Honest,
    /// A re-homed catalogue action, identified by its action-registry `kind` + params.
    /// `contribute()` writes the leaf's slice of ControlSignal (e.g. the equivocator sets
    /// `out.praos.production = Equivocate{ways}` and `out.praos.outbound =
    /// EquivocateRouting{..}`).
    Registered(ActionSpec),
    // Future: NetworkShape { target, delay_ms, drop_rate }, TxGenerator { … }, …
}

// Each leaf contributes to the running ControlSignal; no return-value flow control.
trait LeafAction {
    fn contribute(&mut self, ctx: &TickCtx, out: &mut ControlSignal) -> Status;
}
```

`Registered(ActionSpec)` uses the action registry (`ActionSpec`, formerly `BehaviourSpec`)
as the action-kind discriminant + params. Each shipped adversary (`rb-header-equivocator`,
`lazy-voter`, `t22`, `deep-reorg`, `drop-inbound-peers`) becomes a `LeafAction` whose
`contribute` writes the matching `ControlSignal` fields. Composition is expressed by the BT
structure (`Join`/`Sequence`), not a `composite` leaf.

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

Validation: every `Env(name)` must resolve in the merged `[env]` (an undefined reference
is a hard load-time error); every `Chain(name)` must be a known `NativeChainState` field;
type mismatches (string vs. u64) are load-time errors.

## Configuration (TOML → types)

### `BtConfig`
The on-disk form (see `contracts/bt-config.schema.md` for the full schema, composition
rule, and the canonical worked example). The whole document is keyed tables; the run's
identity + entry live in a top-level `[run]` block; behaviours are id-keyed
(`[behaviours.<id>]`, or `[behaviours.<owner>.<local>]` for a multi-behaviour module); env
keys may be owner-namespaced (D11).

```rust
pub struct BtConfig {
    pub run: Option<Run>,                         // exactly one resolved; supplied by the root
    pub env: BTreeMap<String, EnvValue>,          // dotted keys: shared `x` or owner `owner.x`
    pub behaviours: BTreeMap<BehaviourId, RawBehaviour>,    // [behaviours.<id>] (BehaviourId may be dotted)
    pub metadata: BTreeMap<String, ModuleMeta>,   // optional per-owner docs
    pub includes: Vec<String>,                    // relative paths to sub-behaviour TOMLs
}
pub struct Run { pub name: String, pub seed: u64, pub root: BehaviourId }
pub struct ModuleMeta { pub revision: u32 /* + description, … */ }
```

- `BtConfig::load(path)` resolves `includes` relative to `path` by a **single uniform
  rule** (D13): deep-merge the document and its includes table-by-table, closer-to-root
  wins (no per-section special handling). It detects cycles (behaviour graph + include graph),
  then `validate()`s and compiles to `BehaviourTree`.
- `EnvValue` is a small typed union (`U64`/`F64`/`Str`/`Bool`) so conditions can
  type-check references.

## Validation rules (spec FR-013) — all enforced at load, before activation

1. Exactly one resolved `[run]` carrying `seed` and `root`; `run.root` names a defined
   behaviour (that behaviour is the root). `[run]` set in an included fragment is flagged (lint).
2. Every `children` / include reference resolves to a defined behaviour / readable file.
3. No cycles in the behaviour graph or the include graph.
4. Every behaviour `type` is known; composites have ≥1 child; leaves have none.
5. Every `Condition` references only env keys present in the merged `[env]` and known
   chain-state fields, with matching types. A **referenced-but-undefined `env.X` is an
   error** (D13).
6. Every `Action` resolves to a known action via the registry (e.g. an `Action`
   behaviour's `spec` deserialises into a known `ActionSpec` `kind`) and its `contribute`
   maps to representable `ControlSignal` fields.
7. `run.seed` is present (reproducibility, FR-009) and root-owned.

Note: a same-id behaviour or env key across files is **not** an error — it deep-merges
(closer-to-root wins), the same overlay rule as every other key. Authors namespace by
owner (`[behaviours.<owner>...]`, `[env.<owner>]`) to avoid unintended collisions.

Failures return a precise `Result::Err` with the offending id/path/field; the node
refuses to start (US1 scenario 5) or the REST replace is rejected while the prior tree
stays active (US2 scenario 3/4).

## Relationships

```text
BtConfig --load/validate--> BehaviourTree
BehaviourTree.behaviours[id] = Behaviour{ kind }
BehaviourKind::Action(Registered(spec)) --action registry build(kind)--> LeafAction
tick(ctx) --accumulates contribute()--> ControlSignal
net-node main loop --apply_control--> leios/praos/mempool state (vote policy, tx filter, …)
net-node main loop --publish--> ControlSignal snapshot (arc-swap/watch)
   --read by--> production.rs (production/body_path) + server_handlers.rs (outbound) + main.rs (reorg/drop)
EnvHandle <--writes-- net-node REST handler (DEFERRED / post-MVP, Docker)
NativeChainState <--built each tick from-- slot_clock + mempool (net-node main loop)
```

## State transitions (per behaviour, across ticks)

- **Reactive**: `Sequence`/`Selector` carry **no** resume cursor — each tick re-evaluates
  from the first child (so preconditions are re-checked and can `halt` a running subtree).
  The only state persisted between ticks is a `Join`'s set of already-succeeded children
  and an `Action`'s own internal progress. See
  [`design/bt-grammar-and-semantics.md`](./design/bt-grammar-and-semantics.md) §"State".
- The whole-tree effect is the `ControlSignal` produced this tick. Because the actuators
  are pure reads of the latest `ControlSignal`, a leaf that stops being active simply stops
  contributing, and the next tick's `ControlSignal` reverts those fields to default — there
  is no separate "deactivate" call to make. Honest = `ControlSignal::default()`.
- ControlSignal are recomputed every tick, so there is no stale activation to diff; the
  published snapshot is replaced wholesale each slot.
