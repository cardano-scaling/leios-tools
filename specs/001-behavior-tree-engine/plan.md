# Implementation Plan: Behavior Tree Engine for Adversarial Nodes

**Branch**: `001-behavior-tree-engine` | **Date**: 2026-06-15 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/001-behavior-tree-engine/spec.md`

## Summary

A Behavior Tree (BT) engine is the single abstraction for adversarial node behaviour.
Trees are TOML, ticked once per slot from the node's `SlotClock`, returning
`Success`/`Failure`/`Running`. Composite behaviours (`Sequence`, `Selector`, `Join`) and
`Condition`s gate leaf `Action`s.

The BT tick is the only place decisions are made and emits a typed `ControlSignal`;
consensus actuators read it (the protocol is reactive, so some actuation is event-time, but
decision-free). This deletes the old `Behaviour` hook trait, its outcome types,
`CompositeBehaviour`, and `invoke_hook`. The `ActionSpec` **action registry** is kept as the
leaf lookup; the shipped attack mechanics are re-homed as control-signal contributors.
Determinism preserved (sans-IO core, seeded). Architecture:
[`design/unified-tick-model.md`](./design/unified-tick-model.md); rationale: research D2.

Scope (confirmed): engine core in `shared-rs/consensus` (sim-rs-ready, not integrated);
MVP = honest leaf + 1–2 real demo actions; conditions limited to comparisons/boolean/
membership; **static config only** for the MVP (no REST yet; stdin hot-swap retired). The
feature also splits net-cluster topology configs from BT configs under
`net-cluster/behaviours/`. Grammar & semantics: `design/bt-grammar-and-semantics.md`;
config format & composition: `contracts/bt-config.schema.md` + research D11–D13.

## Technical Context

**Language/Version**: Rust (stable, 2021 edition) — workspace pins stable; no nightly.

**Primary Dependencies**: `serde` + `toml` (config), `figment` (layered load, already
used by net-cluster/net-node), `blake2b_simd` (deterministic seeding, already a crate
convention), and a cheap shared snapshot for the published `ControlSignal` (e.g.
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
  lookup; replaces dynamic-dispatch hooks with a plain `ControlSignal` value (simpler to
  reason about); type-driven behaviour model; `Result` for validation; documented `unsafe` =
  none expected. Follows net-rs `CLAUDE.md` "no panics / simplicity over concision". The
  refactor **removes** code (the hook trait, outcome types, `CompositeBehaviour`,
  `invoke_hook`); each removal is guarded by first porting its behaviour's existing tests
  to the new control-signal contributor (TDD), so no coverage is lost.
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
│   ├── leaf-action.contract.md  # Leaf-action (control-signal contributor) contract
│   └── net-node-rest.md         # net-node env-control REST contract (deferred / post-MVP)
├── design/
│   ├── unified-tick-model.md    # BT architecture (control loop, ControlSignal seam, actuators)
│   └── bt-grammar-and-semantics.md  # BT grammar (EBNF) + operational semantics (tick/halt)
└── checklists/
    └── requirements.md  # Spec quality checklist (already created)
```

### Source Code (repository root)

```text
shared-rs/consensus/src/behaviour/
├── mod.rs                 # REMOVE old Behaviour hook trait + BehaviourOutcome/DecisionOutcome + CompositeBehaviour
├── registry.rs            # KEEP as the ACTION REGISTRY (BehaviourSpec -> ActionSpec): kind -> build(kind, params, seed) -> LeafAction
├── tree/                  # NEW: behaviour-tree engine (sans-IO, deterministic)
│   ├── mod.rs             # BehaviourTree, tick() -> (Status, ControlSignal), Status
│   ├── behaviour.rs       # Behaviour { id, kind }; BehaviourKind: Selector | Sequence | Join | Condition | Action
│   ├── config.rs          # BtConfig: [run] (name/seed/root), [env]/[env.<owner>], [behaviours.<id>], includes; uniform deep-merge
│   ├── env.rs             # DynamicEnv + EnvHandle (Arc<RwLock<DynamicEnv>>); NativeChainState; TickCtx
│   ├── condition.rs       # minimal expression: comparisons, and/or/not, membership
│   ├── control.rs      # ControlSignal { praos, leios, mempool } (the seam)
│   └── actions.rs         # leaf actions as control-signal contributors (honest + re-homed catalogue)
└── actions/               # the action catalogue (formerly behaviours/): re-homed as control-signal contributors

shared-rs/consensus/src/
├── leios.rs               # vote/EB paths read ControlSignal policy fields (no decide_vote/on_* hooks)
├── praos.rs               # block/tip paths read ControlSignal (no on_block_received/on_tip_advanced hooks)
└── mempool.rs             # tx paths read ControlSignal tx_filter (no on_tx_* hooks)

net-rs/net-node/src/
├── main.rs                # slot arm: build NativeChainState, tick BT, apply ControlSignal + publish
├── config.rs              # add behaviour_tree config path; REMOVE stdin behaviour/behaviour_reset
├── bt_runtime.rs          # NEW: holds EnvHandle + BehaviourTree + published ControlSignal snapshot
└── production.rs          # producer reads ControlSignal.production/body_path (no rb_production_strategy hook)

net-rs/net-core/src/peer/
└── server_handlers.rs     # RB-header send reads praos.outbound; serve_leios_notify reads leios.offer_eb_size/echo_to_source (no hooks)

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
and the I/O actuators (`production.rs`, `server_handlers.rs`) read `ControlSignal` instead
of calling hooks. Engine core stays in `shared-rs/consensus` (sim-rs-ready, not coupled);
consumers stay in `net-rs`. This honors the confirmed "shared crate, net-rs wired,
sim-rs-ready" decision and the single-decision-path goal.

## Design Approach (high level)

1. **Engine core (`tree/`)** — pure data + `tick(&mut self, ctx: &TickCtx) ->
   (Status, ControlSignal)`. The tick is the *only* place decisions happen: it
   evaluates `Condition`s over env/state, resolves the active leaf set per composite
   semantics, and accumulates each active leaf's contribution into one `ControlSignal`
   value. Composites are **reactive** (re-evaluate from the first child each tick) with a
   `halt`/abort relation; behaviour kinds are `Sequence` (ordered AND), `Selector` (ordered
   OR), `Join` (concurrent AND, fail-fast), `Condition`, `Action` — full grammar + operational
   semantics in `design/bt-grammar-and-semantics.md`. Validation
   (`BtConfig::validate`) rejects unknown behaviour types, dangling child/include refs,
   cycles, and mistyped env/state refs *before* activation. Seeded RNG from `[metadata]
   seed`.
2. **ControlSignal seam (`control.rs`)** — a plain value the tick emits and the actuators
   consume, **indexed by actuator and domain-grouped** (`ControlSignal { praos, leios,
   mempool }`): production strategy, outbound control signal, reorg/drop, body-path (praos);
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
5. **Leaf actions = control-signal contributors (one file each)** — each leaf, when active,
   contributes to `ControlSignal` (e.g. an equivocator leaf sets `praos.production =
   Equivocate{ways}` and `praos.outbound = EquivocateRouting{..}`). A behaviour owns its
   config struct (+ its own `Deserialize`), its `contribute()`, and its tests **in one
   file**; adding a behaviour touches no other behaviour. Leaves are looked up by `kind`
   via the retained registry (`build`). MVP leaves: an honest contributor (empty
   control signal) plus the re-homed catalogue — `rb-header-equivocator`, `lazy-voter`,
   `t22`, `deep-reorg`, `drop-inbound-peers`, and the merged `lie-about-eb-size`
   (`leios.offer_eb_size`) and `echo-to-source` (`leios.echo_to_source`).
   **Gating house rule**: all flow gating lives in explicit `Condition` behaviours; a leaf
   returns `Running` while active and never branches its status on `env`/`state` (the
   honest fallback returns `Success`).
6. **Actuators read ControlSignal (no hooks)** — the net-node wrapper applies `ControlSignal`
   to the state machines once per slot (e.g. `leios.apply_control(&d)` sets the vote
   policy / tx filter fields) and **publishes** the snapshot (arc-swap / watch) for the
   per-peer send actuators in `server_handlers.rs` — the RB-header send reads
   `praos.outbound`, and `serve_leios_notify` reads `leios.offer_eb_size`/`leios.echo_to_source`
   (preserving the merged `NotificationEntry { source, eb_size }` substrate). Former hook
   sites in `leios`/`praos`/`mempool`/`production`/`server_handlers` become pure reads;
   `Behaviour`/`*Outcome`/`CompositeBehaviour`/`invoke_hook` and the `allow_echo_to_source`/
   `transform_outbound` hooks + the `Outbound`/`OwnedOutbound` enums are deleted.
7. **Tick integration** — in `net-node/src/main.rs`, the existing
   `slot = slot_clock.tick()` arm builds `NativeChainState { current_slot: slot,
   current_epoch, mempool_tx_count }`, ticks the BT, applies + publishes `ControlSignal`.
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

See `design/unified-tick-model.md` for the architecture and
`design/bt-grammar-and-semantics.md` for the grammar/semantics; `research.md` for the
decisions and rationale; `data-model.md` / `contracts/` for the concrete types.

## Complexity Tracking

> No constitution violations — this section intentionally left empty.
