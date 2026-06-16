# Design Decision: Unify behaviours under a single slot-tick BT (Model B)

**Status**: Accepted (2026-06-15) — for discussion with the team; supersedes the
earlier "BT gates, hooks act" sketch in the first plan draft.

**Audience**: engineers working on net-rs adversarial tooling.

This record captures *why* we are replacing the existing `Behaviour` hook trait with
a behavior tree that produces a typed `Directives` value once per slot, and the
analysis that led there. The two tables below (hook catalogue, per-behaviour mapping)
are the evidence; keep them with the decision so the next engineer can re-derive it.

## Problem

We want adversarial node behaviour to be expressed as a **behavior tree** (BT) that is
easy to read and reason about, driven by a single slot tick, with **one** place that
decides anything. The existing system spreads control across many hooks and a
composite short-circuit, which is hard to reason about and would become a second,
parallel decision path next to a BT.

## What the existing hooks do, and when they fire

A `Behaviour` is a trait object consulted by the `LeiosState` / `PraosState` /
`MempoolState` machines and the I/O wrapper. The hooks fire at **two cadences**:

| Hook | Call site | Cadence | Effect |
|---|---|---|---|
| `on_slot_leios` | `leios.rs:575` (`on_slot`) | **slot tick** | per-slot Leios effects |
| `rb_production_strategy` | `net-node main.rs` slot arm | **slot tick** | produce 1 / suppress / equivocate N |
| `praos_reorg` (`maybe_force_reorg`) | `main.rs` slot arm | **slot tick** | force self-reorg this slot |
| `drop_inbound_peers` | `main.rs` slot arm | **slot tick** | reset inbound peers this slot |
| `decide_vote` | `leios.rs:607` (per EB voted) | **sub-tick / event** | override the vote for this EB |
| `transform_outbound` | `net-core server_handlers.rs:179` (per peer send) | **sub-tick / event** | rewrite/drop/replace block to this peer |
| `on_eb_offered` / `on_eb_received` / `on_votes_received` | `leios.rs:832/940/970` | **sub-tick / event** | react to inbound Leios data |
| `on_tx_received` / `on_tx_validated` | `mempool.rs:213` | **sub-tick / event** | react to / rewrite mempool effects |
| `on_block_received` / `on_tip_advanced` | `praos.rs:825/783` | **sub-tick / event** | react to inbound Praos data |
| `decide_body_path` | `net-node production.rs:131` | production time | choose RB body path |

Findings (the questions that drove this decision):

1. **Do hooks act on sub-tick boundaries?** Yes. The RB equivocator's real attack is
   `transform_outbound`, which rewrites the specific block sent to a specific peer at
   *send* time — many times per slot. `decide_vote` overrides per EB; `on_tx_received`
   rewrites mempool effects per tx.
2. **Do hooks make flow-changing decisions?** Yes, in two scattered places: each hook's
   `Continue`/`Replace`/`Append`/`Override` return decides whether honest flow runs, and
   `CompositeBehaviour` short-circuits on "first non-`Continue` wins." That is precisely
   the dual decision path we want to eliminate.
3. **Is a slot-tick BT therefore the only path today?** No — leaf effects are realized
   through event-driven hooks, so a pure slot-tick BT alongside today's hooks would be a
   second path.

## The fundamental limit (not a design choice)

Cardano consensus is **reactive**: EBs, votes, txs, and blocks arrive asynchronously
*within* a slot. A node cannot decide at slot-start what will arrive mid-slot, so a
response to an inbound event must run when the event arrives. **Some actuation is
therefore inherently event-time.** "A single tick drives everything" cannot mean "no
code runs between ticks." It can — and will — mean "only the tick *decides* anything."

## The unification: separate DECISION from ACTUATION

- **Decision / control flow → entirely in the slot tick (the BT).** Conditions,
  selection, sequencing, and "which leaf is active with what parameters" all happen once
  per slot. There is exactly one locus of control: the BT structure.
- **Actuation → mechanical.** At each event-time interception point the code does a
  *pure read* of what the last tick decided and applies it. No branching logic, no
  second brain. It is a dumb effector.

The tick emits a typed **`Directives`** value — the single contract between "decided"
and "applied." Every former hook site becomes "read a field of `Directives`."

### Every current behaviour collapses cleanly into this split

| Behaviour | Decision (moves into the tick) | Actuation (mechanical, at event time) |
|---|---|---|
| LazyVoter | vote policy = abstain(reason) | vote path applies policy; no per-EB decision |
| DropInboundPeers | already slot-tick (per-slot probability draw) | drop flag |
| DeepReorg | already slot-tick (`every_slots`/`depth`) | reorg depth |
| RbHeaderEquivocator | equivocate this slot, `ways=N`, peer→bucket map (pure fn of seed) | send path looks up bucket→variant — a table read, not a decision |
| T22 | filter EBs/TXs by checksum threshold (policy) | filter applies the policy to the datum |

All five reduce to "a decision the tick makes + a mechanical effect." None needs an
independent event-time decision.

## Decision: Model B (Directives), not Model A (trait as effector glue)

Two implementations both achieve a single decision path:

- **Model A — keep the `Behaviour` trait as the actuator interface (least churn).**
  `BehaviourTree` is installed on the existing handle; its `tick()` decides and stores
  activation state; its 15 hook impls become pure reads. Consensus call sites unchanged.
  The 15-method trait survives as dumb glue.
- **Model B — replace the trait with a `Directives` data struct (chosen).** The tick
  produces a plain, readable `Directives` each slot; the consensus / I-O layers read its
  fields directly at their interception points; the `Behaviour` trait, all 15 hooks,
  `BehaviourOutcome` / `DecisionOutcome`, and `CompositeBehaviour` short-circuiting are
  **deleted**. The BT structure (Selector / Sequence / Parallel / Condition) is the only
  control flow; `Directives` is the typed seam. No dynamic dispatch, no second place flow
  can be decided.

**We choose Model B** because the team's priorities are readability, single-path
reasoning, and willingness to refactor. Model B removes the hook-return flow control and
the composite short-circuit entirely — the two things that made the old model hard to
reason about — leaving the BT as the sole decider.

### Cost (accepted)

- ~15 hook call sites change from "call a hook" to "read a `Directives` field" (or read
  a per-slot policy field set on the state machine by the tick).
- The 5 shipped behaviours are re-expressed as **directive contributors** (small; e.g.
  LazyVoter is one line).
- `Behaviour` / `BehaviourOutcome` / `DecisionOutcome` / `CompositeBehaviour` are removed.
- `invoke_hook` plumbing in `leios.rs` / `praos.rs` / `mempool.rs` is removed.

### What we keep

- **The registry** (`ActionSpec`-style tagged enum + `build(kind, params, seed)`):
  retained as the **leaf-action lookup** so a BT config can name a leaf by `kind` and we
  know how to construct its directive contributor. This is the "keep registration" goal.
- **The shipped attack mechanics** (equivocation variant routing, reorg, inbound reset,
  vote abstention, T22 filtering) — re-homed, not redesigned.
- **Determinism**: seed threaded exactly as today (`child_seed` / `seed_from_node_id`);
  no clock reads, no `thread_rng`; `BTreeMap`/`BTreeSet` for ordered state.

## Consequences

- Honest = a trivial one-leaf BT, so there is genuinely one path: everything is a BT.
- The legacy **stdin hot-swap** control (`DynamicConfigUpdate.behaviour` /
  `behaviour_reset`, `Consensus::set_behaviour` / `reset_behaviour`) is retired — the BT
  is set at startup for the MVP. (We are de-prioritising runtime hot-swap.)
- **No REST in the MVP** — we are not running in Docker yet. The net-node REST control
  plane is a later story for when the coordinator must reach nodes across containers
  (stdin can't cross container boundaries; HTTP will).
- The per-event interception points remain in the code (they must, per the reactive
  limit) but contain no decisions — they read `Directives`.

## The seam is owned by actuators, not behaviours (and is domain-grouped)

A subtle but important rule about *who owns what*:

- **A behaviour owns its config + logic + tests, in one file.** Its params struct (with
  its own `#[derive(Deserialize)]`), its `contribute()`, and its `#[cfg(test)]` block all
  live together; the registry (`ActionSpec` variant + `build(kind, params, seed)`) is
  the "find the leaf by name, let it parse its own params" lookup. Adding a behaviour =
  add a file + register its `kind`, with **zero edits to any other behaviour's config**.
- **An actuator owns its directive.** The `Directives` seam is indexed by *actuation
  point* (the one vote decision, the one production strategy, the one per-peer outbound
  transform, the mempool filter), **not** by behaviour — because those are shared,
  singular consensus resources. Multiple active leaves can target the same resource, and
  someone must reconcile them into a single instruction; that reconciliation happens in
  the **tick** (the Model B invariant), so the actuator receives one already-resolved
  value. Keying the seam by behaviour would push that combination logic back into the
  actuator and re-couple consensus to the behaviour catalogue — exactly what we removed.

**Why this does not become a struct everyone edits**: `Directives` is keyed by
capability, not by behaviour, so the *catalogue* can grow without touching it. A new
behaviour that reuses an existing actuator (vote-flipper → `vote`, selective-withholder →
`production`) changes `Directives` **not at all**. You extend `Directives` only when you
add a genuinely new *kind of effect* at a new consensus interception point — which is
rare and unavoidably requires adding the actuator in consensus anyway.

**Domain grouping (decision)**: to avoid one monolithic struct and keep locality,
`Directives` is a thin aggregate of per-domain sub-structs, each owned by its actuator:

```rust
pub struct Directives {
    pub praos:   PraosDirectives,   // production strategy, reorg_depth, outbound, drop_inbound
    pub leios:   LeiosDirectives,   // vote policy, …
    pub mempool: MempoolDirectives, // tx_filter, …
}
```

Domains are few and stable. A new behaviour reusing a capability touches nothing; a new
capability touches exactly one sub-struct + its one actuator. The tick still owns all
reconciliation.

**Mental model**: pair the *behaviour* with its config + `contribute()`; pair the
*actuator* with its directive (sub-)struct. Behaviour ⇄ config+logic (encapsulated, one
file). Actuator ⇄ directive (shared, reconciled in the tick).

## Related config-composition decisions

The config format and multi-file composition are settled in companion decisions (kept out
of this record to keep it focused on the tick/`Directives` architecture):

- **D11** — keyed tables throughout, owner-grouped with one consistent owner word across
  `[metadata.<owner>]`/`[env.<owner>]`/`[behaviours.<owner>...]`; the run's `name`/`seed`/`root`
  live in a top-level `[run]` block.
- **D12** — parameters live in `[env]`/`[env.<owner>]`, referenced by name, with a
  load→CLI→REST overlay precedence ladder.
- **D13** — **one uniform composition rule**: deep-merge the document + its includes,
  closer-to-root wins (no per-section special handling). `[run]` is the only singleton
  (validated: exactly one, root-owned `seed`+`root`); an undefined `env.X` reference is a
  hard load-time error; env is owner-namespaced with a shared tier.

See [`../research.md`](../research.md) D11–D13 and the **canonical worked example** in
[`../contracts/bt-config.schema.md`](../contracts/bt-config.schema.md) §"Worked example"
(a shared-env file + a reusable fragment + a root strategy that includes both and overlays
a parameter).

## Gating style (house rule)

Because a behaviour's tick-time `Status` return can influence its parent's flow, there
are two possible idioms. We adopt the first as the **house rule**, for readability:

- **All gating lives in explicit `Condition` behaviours; leaf actions return `Running` the
  whole time they are meant to be active** (the honest fallback leaf returns `Success`).
  Leaves do **not** branch their status on `env`/`state`. This keeps every flow decision
  in a named, readable `Condition`, so reasoning about "why is this branch active?" means
  reading conditions, never leaf internals.
- (Rejected for house use) letting leaves also return `Success`/`Failure` derived from
  tick-time state — valid classic-BT style, but it spreads flow logic into leaves.

Consequence: a leaf's only job is `contribute()` — write its `Directives` slice and
return `Running` while active. "Stop flooding once the mempool is full" is expressed as a
`Condition` (`cardano.mempool_tx_count < env.trigger_mempool_tx_count`) guarding the
action, **not** as a leaf that inspects state and returns `Success`.

Note this does not change the feedback model: the BT still reacts to the consequences of
actions only by sampling `NativeChainState` at the next tick boundary (via Conditions) —
never via a sub-tick signal.

## Sketch of the seam (see data-model.md for the full types)

```rust
// Produced once per slot by the BT tick; consumed by mechanical actuators.
// Domain-grouped: each sub-struct is owned by its actuator domain.
pub struct Directives {
    pub praos:   PraosDirectives,
    pub leios:   LeiosDirectives,
    pub mempool: MempoolDirectives,
}

pub struct PraosDirectives {
    pub production: RbProductionStrategy, // Normal | Suppress | Equivocate { ways }
    pub outbound: OutboundDirective,      // None | EquivocateRouting{..} | DropTo(set) | ..
    pub reorg_depth: Option<u64>,
    pub drop_inbound: bool,
    pub body_path: Option<BodyPath>,
}
pub struct LeiosDirectives   { pub vote: VotePolicy }       // Honest | Abstain(NoVoteReason)
pub struct MempoolDirectives { pub tx_filter: TxFilterPolicy } // None | ChecksumThreshold{..}

// The whole tree: tick decides, returns status + the slot's directives.
fn tick(&mut self, ctx: &TickCtx) -> (Status, Directives);
```

The net-node wrapper applies `Directives` to the state machines once per slot and
publishes it (arc-swap / watch) for the per-peer send actuator; shared-consensus stays
sans-IO (it only computes the value).
