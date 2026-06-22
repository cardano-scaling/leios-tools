---
description: "Task list for Behavior Tree Engine implementation"
---

# Tasks: Behavior Tree Engine for Adversarial Nodes

**Input**: Design documents in `specs/001-behavior-tree-engine/`
**Prerequisites**: plan.md, spec.md, data-model.md, contracts/, research.md, design/

**Tests**: REQUIRED per the constitution (Principle I, TDD — NON-NEGOTIABLE). Write the
failing test first for every behaviour-bearing task; the only exception is the documented,
human-confirmed manual-test path (Principle II), which is recorded as a task.

**Scope note**: The MVP is **US1 only** (single adversarial node from a static config).
US2 (REST) and US3 (federation) are **deferred** to the Docker/coordination story (spec +
research D8) and are listed as outline-only phases. US4 (sub-behaviour composition) is
satisfied by the foundational config loader + the net-cluster config split — no separate
phase.

## Layout (from plan.md)

- Engine core (sans-IO, deterministic): `shared-rs/consensus/src/behaviour/tree/`
  (`mod.rs`, `behaviour.rs`, `config.rs`, `env.rs`, `condition.rs`, `control.rs`, `actions.rs`)
- Action catalogue: `shared-rs/consensus/src/behaviour/actions/` (re-homed from `behaviours/`)
- Action registry: `shared-rs/consensus/src/behaviour/registry.rs` (`ActionSpec`)
- Consensus actuators: `shared-rs/consensus/src/{leios,praos,mempool,production}.rs`
- Node wiring: `net-rs/net-node/src/{main,config,bt_runtime,production}.rs`
- Outbound actuator: `net-rs/net-core/src/peer/server_handlers.rs`
- Cluster: `net-rs/net-cluster/{behaviours/,configs/,src/config.rs,src/process.rs,src/main.rs}`

Gate per workspace before each commit: `cargo test`, `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`. `shared-rs` writes from a net-rs worktree need
`dangerouslyDisableSandbox`.

---

## Phase 1: Setup

- [X] T001 Create the BT engine module skeleton: add empty `mod.rs`, `behaviour.rs`, `config.rs`, `env.rs`, `condition.rs`, `control.rs`, `actions.rs` under `shared-rs/consensus/src/behaviour/tree/`, and `pub mod tree;` in `shared-rs/consensus/src/behaviour/mod.rs`; confirm `cd shared-rs && cargo build -p shared-consensus` compiles.
- [X] T002 [P] Record the green quality-gate baseline (`cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`) for `shared-rs/` and `net-rs/` before changes. **FINDING (rustc 1.93.0, no toolchain pin): the baseline is NOT green — `cargo test` passes, but `cargo fmt --check` and `cargo clippy -D warnings` fail on PRE-EXISTING toolchain drift in `leios.rs`/`praos.rs`/`mempool.rs`/`behaviours/t22.rs` (fmt import-order) and 2 clippy errors in `mempool.rs` (`clone_on_copy`, `len_without_is_empty`). None are introduced by this work and these files were left untouched; the BT engine code is independently fmt-clean and clippy-clean under `-D warnings` (verified with the 2 pre-existing lints suppressed). Greening the baseline is deferred for human review (CI may pin a different toolchain).**

---

## Phase 2: Foundational — pure engine (sans-IO, deterministic)

**Purpose**: the complete BT engine, unit-tested in isolation, before any consensus
rewiring. Blocks all user stories. The engine loads **self-contained** configs only;
US4 composition (`includes`) is resolved at build time by `bt.py --resolve` (D13
amendment), so the engine's job here is to **reject** unresolved `includes` and compile a
flat config.

**⚠️ Leaves the existing `Behaviour` hook system in place** so the crate stays green; the old
system is deleted in Phase 3 (T025).

- [X] T003 [P] Define `Status { Success, Failure, Running }` in `shared-rs/consensus/src/behaviour/tree/mod.rs` with a unit test (FR-001).
- [X] T004 [P] Define the control-signal seam in `shared-rs/consensus/src/behaviour/tree/control.rs`: `ControlSignal { praos, leios, mempool }`, `PraosControl`/`LeiosControl`/`MempoolControl`, `VotePolicy`, `OutboundControl`, `TxFilterPolicy`, and `EbSizePolicy` (for the merged `lie-about-eb-size`). `LeiosControl` carries `vote`, `offer_eb_size: EbSizePolicy`, and `echo_to_source: bool`. Reuse existing `RbProductionStrategy`/`BodyPath`/`NoVoteReason`; test that `Default` is honest.
- [X] T005 [P] Define `DynamicEnv(BTreeMap<String, EnvValue>)`, `EnvValue`, `EnvHandle`, `NativeChainState`, and `TickCtx` in `shared-rs/consensus/src/behaviour/tree/env.rs`; test typed get/insert and dotted (owner-namespaced) keys.
- [X] T006 Write failing tests for the minimal condition grammar in `shared-rs/consensus/src/behaviour/tree/condition.rs`: comparisons, `and`/`or`/`not`, `contains(...)`, `env.*`/`cardano.*` refs, and load-time errors for undefined refs and type mismatch (contracts/bt-config.schema.md).
- [X] T007 Implement `ConditionExpr` parse + evaluate in `shared-rs/consensus/src/behaviour/tree/condition.rs` to pass T006.
- [X] T008 Write failing tests for tick/halt semantics in `shared-rs/consensus/src/behaviour/tree/behaviour.rs` per design §5: `Sequence` (ordered AND), `Selector` (ordered OR), `Join` (fail-fast + succeeded-set), `ForTicks` (elapsed cap + reset on halt), `Condition`, reactive re-evaluation, and the reactive-abort `halt`.
- [X] T009 Implement `Behaviour`, `BehaviourKind` (`Selector`/`Sequence`/`Join`/`ForTicks`/`Condition`/`Action`), `BehaviourTree::tick(&TickCtx) -> (Status, ControlSignal)`, and `halt` in `shared-rs/consensus/src/behaviour/tree/{behaviour.rs,mod.rs}` to pass T008.
- [X] T010 Write failing tests, then implement, the `LeafAction` trait (`contribute(&mut self, &TickCtx, &mut ControlSignal) -> Status`), `ActionKind { Honest, Registered(ActionSpec) }`, and an action-registry `build_action(spec, seed) -> Box<dyn LeafAction>` in `shared-rs/consensus/src/behaviour/tree/actions.rs` + `registry.rs` (`ActionSpec`, alongside the existing `BehaviourSpec` for now).
- [X] T011 [P] [US4-support] Re-home `lazy-voter` as a `LeafAction` (sets `leios.vote = Abstain(reason)`) in `shared-rs/consensus/src/behaviour/actions/lazy_voter.rs`; test-first.
- [X] T012 [P] [US4-support] Re-home `rb-header-equivocator` (sets `praos.production = Equivocate{ways}` + `praos.outbound = EquivocateRouting`) in `shared-rs/consensus/src/behaviour/actions/rb_equivocator.rs`; test-first (incl. deterministic peer-bucket routing).
- [X] T013 [P] [US4-support] Re-home `deep-reorg` (sets `praos.reorg_depth` on due slots; periodicity self-gated from `(seed, slot)`) in `shared-rs/consensus/src/behaviour/actions/deep_reorg.rs`; test-first.
- [X] T014 [P] [US4-support] Re-home `drop-inbound-peers` (sets `praos.drop_inbound` from the seeded per-slot draw) in `shared-rs/consensus/src/behaviour/actions/drop_inbound.rs`; test-first.
- [X] T015 [P] [US4-support] Re-home `t22` (sets `mempool.tx_filter = ChecksumThreshold{..}`) in `shared-rs/consensus/src/behaviour/actions/t22.rs`; test-first.
- [X] T015a [P] [US4-support] Re-home the merged `lie-about-eb-size` (sets `leios.offer_eb_size = Linear{scale_num, scale_den, offset}`; reuse the i128 `mutate_size` math) in `shared-rs/consensus/src/behaviour/actions/lie_about_eb_size.rs`; port the merged size-math unit tests.
- [X] T015b [P] [US4-support] Re-home the merged `echo-to-source` (sets `leios.echo_to_source = true`) in `shared-rs/consensus/src/behaviour/actions/echo_to_source.rs`; port the merged trait-wiring tests.
- [X] T016 Write failing tests for `BtConfig::load` in `shared-rs/consensus/src/behaviour/tree/config.rs`: parse `[run]`/`[env]`/`[behaviours.<id>]` from a **self-contained** config; name resolution with **expansion to independent instances**; **reject a non-empty `includes`** with a clear "run `bt.py --resolve`" error (the engine does not resolve includes — D13 amendment); and every validation rule (exactly one `[run]`; dangling child reference; behaviour-graph reference cycles; undefined `env.X`; type mismatch).
- [X] T017 Implement `BtConfig` + `load`/`validate`/compile-to-`BehaviourTree` in `shared-rs/consensus/src/behaviour/tree/config.rs` to pass T016 (parse one flat config, reject unresolved `includes`, reference expansion). Cross-file `includes` are resolved upstream by `bt.py --resolve`; US4 composition is delivered by that translator step.
- [X] T018 Add a determinism test in `shared-rs/consensus/src/behaviour/tree/`: same config + seed ⇒ identical `(Status, ControlSignal)` sequence over a slot range (SC-003, FR-023).

**Checkpoint**: engine compiles, all engine unit tests green; `fmt`/`clippy` clean on `shared-rs/`.

---

## Phase 3: User Story 1 — single adversarial node from a static config (Priority: P1) 🎯 MVP

**Goal**: launch one `net-node` with a static BT config; it behaves honestly until the
trigger condition, then runs the adversarial branch.

**Independent test**: run `net-node --behaviour-tree <config>` with a slot-trigger config and
observe (telemetry) honest behaviour before the trigger slot and the adversarial branch
at/after it, with no REST or coordinator (quickstart Scenario 2).

### Tests for User Story 1 (REQUIRED — TDD, NON-NEGOTIABLE) ⚠️

- [ ] T019 [US1] Write failing integration tests: (a) a loaded tree ticked across slots is honest before `env.trigger_slot` and adversarial at/after (SC-002); (b) every config in a malformed-config suite is rejected at load with a precise error and 0% partially run (SC-004). Place in `shared-rs/consensus/src/behaviour/tree/` (engine integration) and `net-rs/net-node/` (node integration).

### Implementation for User Story 1

- [X] T020 [US1] Add `apply_control(&ControlSignal)` to `LeiosState`/`PraosState`/`MempoolState` in `shared-rs/consensus/src/{leios,praos,mempool}.rs` storing per-domain policy fields read by the actuators; test-first.
- [X] T021 [US1] Rewire the Leios vote path in `shared-rs/consensus/src/leios.rs` to apply `VotePolicy` (remove the `decide_vote` hook call + `invoke_hook`); test. **Note: vote-decision now reads `control.leios.vote`; the shared `invoke_hook`/reactive-hook infra is removed in the bulk T025 deletion.**
- [ ] T022 [US1] Rewire `shared-rs/consensus/src/praos.rs` (block/tip paths) to drop `on_block_received`/`on_tip_advanced` hook calls; reorg/drop read from `ControlSignal`; test.
- [X] T023 [US1] Rewire the mempool path in `shared-rs/consensus/src/mempool.rs` to apply `TxFilterPolicy` (remove `on_tx_*` hook calls); test. **Done: t22's `ChecksumThreshold` filter actuates in `LeiosState::on_eb_offered`/`on_eb_txs_offered`/`on_eb_received` reading `control.mempool.tx_filter` (the EB-processing path, where t22 originally hooked); checksum ported verbatim; 5 actuator tests. The no-op `on_tx_*` mempool reactive hooks are removed in the bulk T025 deletion.**
- [ ] T024 [US1] Rewire production: `shared-rs/consensus/src/production.rs` body-path and `net-rs/net-node/src/production.rs` to read `praos.production`/`praos.body_path` from the published `ControlSignal` (remove `rb_production_strategy`/`decide_body_path` hooks); test.
- [ ] T025 [US1] Delete the `Behaviour` hook trait and all its hooks (incl. the merged `allow_echo_to_source`/`transform_outbound`), `BehaviourOutcome`/`DecisionOutcome`, `CompositeBehaviour`, `invoke_hook`, the `Outbound`/`OwnedOutbound` enums, and the old per-behaviour `impl Behaviour` blocks from `shared-rs/consensus/src/behaviour/`; collapse `registry.rs` to `ActionSpec` + `build_action` only; ensure `shared-rs` + `net-rs` compile and all tests pass.
- [ ] T026 [US1] Create `net-rs/net-node/src/bt_runtime.rs`: holds the `EnvHandle` + `BehaviourTree` and publishes the per-slot `ControlSignal` via a cheap shared cell (`arc-swap`/`tokio::watch`); test-first.
- [ ] T027 [US1] Wire the tick into `net-rs/net-node/src/main.rs` slot arm: build `NativeChainState` (current slot/epoch, mempool tx count), tick the BT, `apply_control` to the state machines, publish the snapshot; remove the stdin `behaviour`/`behaviour_reset` handling.
- [ ] T028 [US1] Add `--behaviour-tree <path>` (and a `behaviour_tree` config key) to `net-rs/net-node/src/config.rs`; load + compile the `BtConfig`; default to an implicit honest one-leaf tree when absent; remove the `behaviour`/`behaviour_reset` config fields; test (incl. start-up refusal on an invalid config, US1 scenario 5).
- [ ] T029 [US1] Rewire the RB-header per-peer outbound actuator in `net-rs/net-core/src/peer/server_handlers.rs` to read the published `praos.outbound` (equivocation routing / drop) instead of `transform_outbound`; test.
- [ ] T029a [US1] Rewire the LeiosNotify offer actuator `serve_leios_notify` in `net-rs/net-core/src/peer/server_handlers.rs` to read the published `leios.offer_eb_size` (rewrite `eb_size`) and `leios.echo_to_source` (no-echo gate) per offer entry, instead of `allow_echo_to_source`/`transform_outbound`; preserve the `NotificationEntry { source, eb_size }` substrate; port the merged 3 mux-pair integration tests.
- [ ] T030 [US1] Emit telemetry of the active behaviour(s) and per-tick status in `net-rs/net-node/src/telemetry.rs` + `main.rs` (FR-015); test where automatable.
- [ ] T031 [US1] [US4] Config split in `net-rs/net-cluster/`: extract each `[behaviour]`/`[behaviour_selection]` block from `configs/*.toml` into named BT configs under `net-cluster/behaviours/`; reference them by name from the (topology-only) configs; update `src/config.rs` + `src/process.rs` to distribute the referenced BT config to the selected nodes at spawn (keep a deprecation shim for inline `[behaviour]`); tests for load + round-trip.
- [ ] T032 [P] [US1] Author sample BT configs under `net-rs/net-cluster/behaviours/` (honest + the re-homed catalogue), including the duplex-follower-bug `Join["echo-to-source", lie-about-eb-size(0,1,0)]` example, and a single-node `--behaviour-tree` example, matching `contracts/bt-config.schema.md`.
- [ ] T033 [US1] Run quickstart Scenario 2 (single-node honest→adversarial switch) and Scenario 3 (config-split round-trip); automate what's feasible and record the human-confirmed manual result (Principle II).

**Checkpoint**: MVP complete — a single net-node runs an adversarial BT from a static config.

---

## Phase 4: User Story 2 — REST control (Priority: P2) — DEFERRED (post-MVP / Docker)

Outline only (research D8): add an axum control surface to `net-node` (mirroring
`net-cluster/src/server.rs`) to `GET` the active config/env, `PUT /api/bt/env/:key`, and
`POST /api/bt/config` (validated, atomic), writing the `EnvHandle`. Detailed tasks when the
Docker/coordination work begins. Contract: `contracts/net-node-rest.md`.

## Phase 5: User Story 3 — federation (Priority: P3) — DEFERRED (post-MVP / Docker)

Outline only: the `net-cluster` coordinator distributes BT configs and per-node env over the
US2 REST surface, reporting per-node success/failure. Builds on US2.

---

## Phase 6: Polish & Cross-Cutting

- [ ] T034 [P] Security/robustness audit of new untrusted-input paths (config parsing, env REST later): allocation/recursion bounds on tree depth / reference expansion; no panics in non-test code (`unwrap`/`expect`/index) per net-rs `CLAUDE.md`. (Include-resolution bounds — cycle/recursion limits — belong to `bt.py --resolve`, the build-time resolver.)
- [ ] T035 [P] Update `net-rs/CLAUDE.md` and `shared-rs/consensus/CLAUDE.md` to describe the BT engine, `ControlSignal`, and the action registry; remove stale references to the deleted hook trait.
- [ ] T036 Run the full quickstart validation suite (Scenarios 1–3) and the per-workspace gate (`cargo test` + `fmt --check` + `clippy -D warnings`) on `shared-rs/` and `net-rs/`.

---

## Dependencies & Execution Order

- **Setup (P1)** → no deps.
- **Foundational (P2)** → depends on Setup; **blocks** all stories. Within P2: T003/T004/T005 [P] first; T006→T007; T008→T009; T010 then T011–T015 + T015a/T015b [P]; T016→T017; T018 last. T015a/T015b depend on T004's `EbSizePolicy`/`LeiosControl` fields.
- **US1 (P3)** → depends on P2 complete. T019 (tests) first. T020 before T021–T024. T025 (delete old system) after T021–T024 and after the actions/ contributors (T011–T015, T015a/b) exist. T026 before T027. T029/T029a (outbound actuators) after T026. T027/T028 before T033. T031 independent of consensus rewiring (cluster-side).
- **US2 (P4)** → depends on US1. **US3 (P5)** → depends on US2.
- **Polish (P6)** → after US1 (MVP) at minimum.

## Parallel Opportunities

- Setup: T002 ∥ T001 follow-up.
- P2: T003 ∥ T004 ∥ T005 (distinct files); the seven re-homings T011 ∥ T012 ∥ T013 ∥ T014 ∥ T015 ∥ T015a ∥ T015b (distinct files).
- US1: T032 (sample configs) ∥ consensus rewiring; T031 (cluster split) largely ∥ the net-node wiring.
- Polish: T034 ∥ T035.

## Implementation Strategy

**MVP = Phase 1 + Phase 2 + Phase 3 (US1).** Complete the pure engine (P2) and validate it in
isolation, then wire it into net-node and delete the old hook system (P3), then stop and
validate the single-node honest→adversarial switch. US2/US3 (REST, federation) follow only
when the Docker/coordination work starts.
