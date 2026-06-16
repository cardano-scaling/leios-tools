# Implementation Plan: Behavior Tree Engine for Adversarial Nodes

**Branch**: `001-behavior-tree-engine` | **Date**: 2026-06-15 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/001-behavior-tree-engine/spec.md`

## Summary

Add a Behavior Tree (BT) engine that is the **single abstraction** for adversarial
node behaviour. Trees are described in TOML, ticked once per slot advance from the
node's existing `SlotClock`, and return `Success`/`Failure`/`Running`. Composite behaviours
(`Selector`, `Sequence`, `Parallel`) and `Condition` behaviours gate leaf `Action` behaviours.

**Architecture decision (Model B — see
[`design/unified-tick-model.md`](./design/unified-tick-model.md))**: we replace the
existing `Behaviour` hook trait with a BT that produces a typed `Directives` value once
per slot. The decisive analysis is the separation of **decision** from **actuation**:

- **Decision / control flow happens only in the slot tick (the BT).** There is exactly
  one place anything is decided.
- **Actuation is mechanical.** Cardano consensus is reactive — EBs, votes, txs, and
  blocks arrive asynchronously *within* a slot — so some effects must be applied at
  event-time interception points (e.g. per-peer block send, per-EB vote). Those points
  remain, but they contain **no decisions**: each does a pure read of the `Directives`
  the last tick produced and applies it.

This deletes the hook-return flow control (`BehaviourOutcome`/`DecisionOutcome`),
`CompositeBehaviour` short-circuiting, the 15-hook `Behaviour` trait, and the
`invoke_hook` plumbing — the parts that made the old model hard to reason about. The BT
structure becomes the sole locus of control; `Directives` is the typed seam between
"decided" and "applied."

**What we keep**: the **registry** (`ActionSpec`-style tagged enum + `build(kind,
params, seed)`) is retained as the **leaf-action lookup**, so a BT config names a leaf
by `kind` and we know how to construct its directive contributor. The shipped attack
mechanics (equivocation variant routing, reorg, inbound reset, vote abstention, T22
filtering) are re-homed as directive contributors, not redesigned. Determinism is
preserved (seed threaded as today; sans-IO core).

Scope is set by the decisions confirmed with the user: engine core lives in the shared
`consensus` crate (sim-rs-ready, not sim-rs-integrated); the MVP ships an honest leaf
plus 1–2 real demo actions re-expressed as directive contributors; condition
expressions are limited to comparisons, boolean combinators, and simple membership.
The MVP control plane is **static config only** — no REST (we are not in Docker yet),
and the legacy **stdin hot-swap** behaviour-control path is retired.

This feature also performs the **config split** the user requested: the per-node
`[behaviour]` / `[behaviour_selection]` blocks are extracted out of the
`net-cluster/configs/*.toml` topology files into standalone BT configs under
`net-cluster/behaviours/`, referenced by name. Topology files keep only topology +
initial network-shaping characteristics.

## Technical Context

**Language/Version**: Rust (stable, 2021 edition) — workspace pins stable; no nightly.

**Primary Dependencies**: `serde` + `toml` (config), `figment` (layered load, already
used by net-cluster/net-node), `blake2b_simd` (deterministic seeding, already a crate
convention), and a cheap shared snapshot for the published `Directives` (e.g.
`arc-swap`, or reuse the existing `tokio::sync::watch` pattern). No `axum`/REST in the
MVP. No new heavyweight or C-binding dependencies. A general expression-DSL crate is
explicitly **not** adopted (minimal condition grammar — see research).

**Storage**: Filesystem TOML only — BT configs under `net-cluster/behaviours/` and a
single-file static config passed to net-node via `--config`. No database.

**Testing**: `cargo test` (unit + integration), per workspace. `shared-rs` for the
engine core; `net-rs` for node/cluster integration and the REST surface (axum
`oneshot` tests as in `net-cluster/src/server.rs`).

**Target Platform**: Linux/macOS dev hosts; same as the existing workspaces.

**Project Type**: Multi-crate Rust workspaces (`shared-rs`, `net-rs`). Engine core in
`shared-rs/consensus`; consumers in `net-rs` (`net-node`, `net-cluster`).

**Performance Goals**: Tick cost is negligible against a ~1s slot; a tick must
complete well within one slot for trees up to dozens of nodes. No hot-path
allocation requirements beyond the existing per-slot loop budget.

**Constraints**: `shared-consensus` is **sans-IO and deterministic** (no `tokio`, no
clock reads, no `thread_rng`, `BTreeMap`/`BTreeSet` not `HashMap` in ordered paths).
The BT core must obey this: it reads injected `&NativeChainState` and a guarded
`DynamicEnv`, returns effects, and seeds any randomness from the config seed. No
panics in non-test code (net-rs `CLAUDE.md` rule): every `unwrap`/`expect`/index must
be justified or replaced with `Result`/`Option`.

**Scale/Scope**: Trees of up to a few dozen nodes; a federation of tens of net-nodes
under one net-cluster coordinator. MVP is a single node from a static config.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.0.0 principles applied to this plan:

- **I. Test-Driven Development (NON-NEGOTIABLE)**: PASS (by construction). Every task
  in `tasks.md` will write a failing test first. The engine core is pure and trivially
  unit-testable (tick a tree, assert status + effects); the REST surface uses axum
  `oneshot` tests; the config split is covered by load/round-trip tests. No production
  code lands without a test or a recorded human-confirmed manual test (Principle II).
- **II. Verified Test Coverage**: PASS. Automated tests are the default. The only
  manual-confirmation candidates are live multi-node cluster demos (quickstart.md);
  those will be recorded and human-confirmed, not asserted silently.
- **III. Adversarial Red-Team Focus**: PASS. The feature *is* adversarial tooling.
  Tests will include malformed/hostile configs (unknown behaviour types, dangling child
  refs, cyclic includes, mistyped env refs), slot-skip and RUNNING-across-ticks edge
  cases, and reproduced honest→adversarial transitions.
- **IV. Idiomatic Rust**: PASS. Retains the `ActionSpec` registry idiom for leaf
  lookup; replaces dynamic-dispatch hooks with a plain `Directives` value (simpler to
  reason about); type-driven behaviour model; `Result` for validation; documented `unsafe` =
  none expected. Follows net-rs `CLAUDE.md` "no panics / simplicity over concision". The
  Model B refactor **removes** code (the hook trait, outcome types, `CompositeBehaviour`,
  `invoke_hook`); each removal is guarded by first porting its behaviour's existing tests
  to the new directive contributor (TDD), so no coverage is lost.
- **V. Automated Quality Gates**: PASS. `cargo fmt --check` + `cargo clippy` (no
  warnings) + `cargo test` gate every commit, per affected workspace (`shared-rs`,
  `net-rs`). Note: `shared-rs` writes from a net-rs worktree need
  `dangerouslyDisableSandbox` per the shared-consensus CLAUDE.md.

**Result**: No violations. No entries required in Complexity Tracking.

## Project Structure

### Documentation (this feature)

```text
specs/001-behavior-tree-engine/
├── plan.md              # This file
├── spec.md              # Feature specification
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── bt-config.schema.md      # BT TOML config contract
│   ├── leaf-action.contract.md  # Leaf-action (directive contributor) contract
│   └── net-node-rest.md         # net-node env-control REST contract (deferred / post-MVP)
├── design/
│   └── unified-tick-model.md    # Model B decision record (hook catalogue + tables)
└── checklists/
    └── requirements.md  # Spec quality checklist (already created)
```

### Source Code (repository root)

```text
shared-rs/consensus/src/behaviour/
├── mod.rs                 # REMOVE old Behaviour hook trait + BehaviourOutcome/DecisionOutcome + CompositeBehaviour
├── registry.rs            # KEEP as the ACTION REGISTRY (BehaviourSpec -> ActionSpec): kind -> build(kind, params, seed) -> LeafAction
├── tree/                  # NEW: behaviour-tree engine (sans-IO, deterministic)
│   ├── mod.rs             # BehaviourTree, tick() -> (Status, Directives), Status
│   ├── behaviour.rs       # Behaviour { id, kind }; BehaviourKind: Selector | Sequence | Parallel | Condition | Action
│   ├── config.rs          # BtConfig: [run] (name/seed/root), [env]/[env.<owner>], [behaviours.<id>], includes; uniform deep-merge
│   ├── env.rs             # DynamicEnv + EnvHandle (Arc<RwLock<DynamicEnv>>); NativeChainState; TickCtx
│   ├── condition.rs       # minimal expression: comparisons, and/or/not, membership
│   ├── directives.rs      # Directives { praos, leios, mempool } (the seam)
│   └── actions.rs         # leaf actions as directive contributors (honest + re-homed catalogue)
└── actions/               # the action catalogue (formerly behaviours/): re-homed as directive contributors

shared-rs/consensus/src/
├── leios.rs               # vote/EB paths read Directives policy fields (no decide_vote/on_* hooks)
├── praos.rs               # block/tip paths read Directives (no on_block_received/on_tip_advanced hooks)
└── mempool.rs             # tx paths read Directives tx_filter (no on_tx_* hooks)

net-rs/net-node/src/
├── main.rs                # slot arm: build NativeChainState, tick BT, apply Directives + publish
├── config.rs              # add behaviour_tree config path; REMOVE stdin behaviour/behaviour_reset
├── bt_runtime.rs          # NEW: holds EnvHandle + BehaviourTree + published Directives snapshot
└── production.rs          # producer reads Directives.production/body_path (no rb_production_strategy hook)

net-rs/net-core/src/peer/
└── server_handlers.rs     # per-peer send reads published Directives.outbound (no transform_outbound hook)

net-rs/net-cluster/
├── behaviours/            # NEW: extracted BT configs (peer dir to configs/)
│   ├── honest.toml
│   ├── rb-equivocator.toml
│   └── ...                # one per former [behaviour] block
├── configs/*.toml         # topology + initial shaping ONLY; reference a behaviour by name
└── src/config.rs          # behaviour now references a BT config; coordinator distributes it
```

**Structure Decision**: Add the BT engine as a `tree/` submodule of the existing
`behaviour/` subsystem (it reuses the `ActionSpec` registry for leaf lookup and the
crate's determinism rules), and **delete** the `Behaviour` hook trait, its outcome
types, and `CompositeBehaviour`. The consensus state machines (`leios`/`praos`/`mempool`)
and the I/O actuators (`production.rs`, `server_handlers.rs`) read `Directives` instead
of calling hooks. Engine core stays in `shared-rs/consensus` (sim-rs-ready, not coupled);
consumers stay in `net-rs`. This honors the confirmed "shared crate, net-rs wired,
sim-rs-ready" decision and Model B's "single decision path" goal.

## Design Approach (high level)

1. **Engine core (`tree/`)** — pure data + `tick(&mut self, ctx: &TickCtx) ->
   (Status, Directives)`. The tick is the *only* place decisions happen: it
   evaluates `Condition`s over env/state, resolves the active leaf set per composite
   semantics, and accumulates each active leaf's contribution into one `Directives`
   value. Composites carry `Running` memory across ticks. Validation
   (`BtConfig::validate`) rejects unknown behaviour types, dangling child/include refs,
   cycles, and mistyped env/state refs *before* activation. Seeded RNG from `[metadata]
   seed`.
2. **Directives seam (`directives.rs`)** — a plain value the tick emits and the actuators
   consume, **indexed by actuator and domain-grouped** (`Directives { praos, leios,
   mempool }`): production strategy, outbound directive, reorg/drop, body-path (praos);
   vote policy (leios); tx filter (mempool). This is the typed contract that replaces the
   deleted hook trait. Each sub-struct is owned by its actuator domain; a behaviour that
   reuses an existing capability never edits it. Conflicts between two active leaves on
   the same field are reconciled deterministically in the tick — never by the actuator.
3. **Config + includes** — `BtConfig` parses a `[run]` block (`name`/`seed`/`root`),
   `[env]`/`[env.<owner>]`, optional `[metadata.<owner>]`, and id-keyed `[behaviours.<id>]`
   (or `[behaviours.<owner>.<local>]`) tables, plus `includes = ["a.toml", ...]`. Composition is
   **one uniform rule** (research D11–D13): deep-merge the document and its includes
   table-by-table, closer-to-root wins (no per-section special handling). The only
   singleton is `[run]` (validated: exactly one, root-owned `seed`+`root`); `[env]` overlay
   precedence is load → `--set` → later REST; a referenced-but-undefined `env.X` is a hard
   load-time error; env is owner-namespaced (`[env.<owner>]`) with a shared tier; cycles
   detected. See the canonical worked example in `contracts/bt-config.schema.md`.
4. **Env guarding** — `DynamicEnv` behind `EnvHandle = Arc<std::sync::RwLock<DynamicEnv>>`
   (std, not tokio — sans-IO). The tick reads it; a later REST surface writes it. For the
   MVP env is set at startup. `NativeChainState` is rebuilt each tick and passed by `&`
   (read-only, unguarded), per the user's note.
5. **Leaf actions = directive contributors (one file each)** — each leaf, when active,
   contributes to `Directives` (e.g. an equivocator leaf sets `praos.production =
   Equivocate{ways}` and `praos.outbound = EquivocateRouting{..}`). A behaviour owns its
   config struct (+ its own `Deserialize`), its `contribute()`, and its tests **in one
   file**; adding a behaviour touches no other behaviour. Leaves are looked up by `kind`
   via the retained registry (`build`). MVP leaves: an honest contributor (empty
   directives) plus 1–2 re-homed real ones (e.g. `rb-header-equivocator`, `lazy-voter`).
   **Gating house rule**: all flow gating lives in explicit `Condition` behaviours; a leaf
   returns `Running` while active and never branches its status on `env`/`state` (the
   honest fallback returns `Success`).
6. **Actuators read Directives (no hooks)** — the net-node wrapper applies `Directives`
   to the state machines once per slot (e.g. `leios.apply_directives(&d)` sets the vote
   policy / tx filter fields) and **publishes** the snapshot (arc-swap / watch) for the
   per-peer send actuator in `server_handlers.rs`. Former hook sites in `leios`/`praos`/
   `mempool`/`production`/`server_handlers` become pure reads. `Behaviour`/`*Outcome`/
   `CompositeBehaviour`/`invoke_hook` are deleted.
7. **Tick integration** — in `net-node/src/main.rs`, the existing
   `slot = slot_clock.tick()` arm builds `NativeChainState { current_slot: slot,
   current_epoch, mempool_tx_count }`, ticks the BT, applies + publishes `Directives`.
   Slot skips produce exactly one tick for the advance (consistent with the existing
   loop).
8. **Config split** — move each `[behaviour]`/`[behaviour_selection]` block out of the
   topology TOMLs into `net-cluster/behaviours/*.toml`; topology configs reference a BT
   config by name; the coordinator distributes the BT config to selected nodes at spawn
   (reusing the existing selection path). Retire the stdin behaviour-swap path.
9. **net-node REST (deferred, post-MVP / Docker)** — when nodes run in containers and
   the coordinator must reach them over the network, add an HTTP control surface
   (mirroring `net-cluster/server.rs`) to read/replace the BT config and mutate env.
   Not in the MVP (no Docker yet).

See `design/unified-tick-model.md` for the full decision record (hook catalogue,
decision-vs-actuation split, per-behaviour mapping, Model A vs B), `research.md` for
the surrounding decisions, and `data-model.md` / `contracts/` for the concrete types.

## Complexity Tracking

> No constitution violations — this section intentionally left empty.
