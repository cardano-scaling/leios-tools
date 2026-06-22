# Phase 0 Research: Behavior Tree Engine

All NEEDS CLARIFICATION items from the spec were resolved with the user (see spec
Assumptions Q1–Q3). This document records the design decisions, their rationale, and
the alternatives considered, grounded in the existing codebase.

The central architecture decision — the **BT evaluator**: replace the `Behaviour` hook
trait with a slot-tick BT that emits a typed `ControlSignal` value — is described in
[`design/unified-tick-model.md`](./design/unified-tick-model.md). D2 and D10 below record
the decision and the rejected alternatives.

## D1. Engine placement: `shared_consensus::behaviour::tree`

- **Decision**: Add the BT engine as a `tree/` submodule under the existing
  `shared-rs/consensus/src/behaviour/` subsystem.
- **Rationale**: The confirmed scope is "shared crate, net-rs wired, sim-rs-ready." The
  BT engine reuses the **registry** (`ActionSpec` + `build`) as its leaf-action
  lookup and the deterministic seeding helpers (`child_seed`, `seed_from_node_id`), and
  it inherits the crate's sans-IO/determinism rules, which the BT must obey anyway.
  sim-rs can later consume the same module unchanged. (Note: the `Behaviour` *trait* and
  its `Arc<Mutex<Box<dyn Behaviour>>>` swap machinery are **removed** by the BT evaluator —
  see D2; what we keep from the old subsystem is the registry and the seeding helpers.)
- **Alternatives considered**:
  - *New `shared-rs/bt` crate* — rejected: would duplicate the determinism rules and
    create a dependency edge back to `consensus` for the registry with no benefit.
  - *Put it directly in `net-node`* — rejected: violates the confirmed shared-crate
    decision and blocks sim-rs reuse.

## D2. Replace the `Behaviour` hook trait with a slot-tick BT emitting `ControlSignal`

- **Decision**: The BT is the **single abstraction** for adversarial behaviour. We
  *delete* the `Behaviour` hook trait, `BehaviourOutcome`/`DecisionOutcome`,
  `CompositeBehaviour`, and the `invoke_hook` plumbing. The BT `tick(&TickCtx) ->
  (Status, ControlSignal)` is the **only** place decisions are made. It emits a typed
  `ControlSignal` value that mechanical actuators read at their interception points.
- **The key analysis — decision vs. actuation**: the existing hooks fire at two
  cadences (slot-tick: `rb_production_strategy`, `praos_reorg`, `drop_inbound_peers`,
  `on_slot_leios`; **sub-tick/event**: `transform_outbound` per peer-send, `decide_vote`
  per EB, `on_tx_received` per tx, `on_*` on arrival). Cardano consensus is *reactive*,
  so a response to an inbound event must run when the event arrives — **some actuation is
  inherently event-time and cannot be moved to the tick**. The resolution: move all
  *decisions* into the tick; leave the event-time interception points as **mechanical
  reads** of the `ControlSignal` the last tick produced. One decision path; dumb effectors.
- **Why this over a "BT gates, hooks act" alternative**: keeping the 15-hook trait as
  effector glue preserves the same runtime with less churn, but the team's priorities are
  readability and a single decision path. The BT evaluator removes the hook-return flow
  control and the `CompositeBehaviour` short-circuit — the two scattered places flow was
  decided — leaving the BT structure as the sole locus of control.
- **What we keep**: the **registry** (`ActionSpec`-style tagged enum + `build(kind,
  params, seed)`) as the leaf-action lookup, and the shipped attack **mechanics**
  (equivocation variant routing, reorg, inbound reset, vote abstention, T22 filtering),
  re-homed as control-signal contributors — not redesigned. Determinism is preserved.
- **Cost (accepted)**: ~15 hook call sites in `leios`/`praos`/`mempool`/`production`/
  `server_handlers` change from "call a hook" to "read a `ControlSignal` field"; the five
  shipped behaviours are re-expressed as control-signal contributors; the trait/outcome types
  and `CompositeBehaviour` are removed. Each removal is guarded by first porting the
  behaviour's existing tests to the new contributor (TDD), so no coverage is lost.
- **Alternatives considered**:
  - *Hook-glue alternative (the `Behaviour` trait survives as effector glue)* — rejected:
    least churn, but keeps a competing concept and dynamic dispatch.
  - *Leaves directly mutate consensus / call `tokio`* — rejected: pulls I/O into a
    sans-IO crate; breaks determinism and testability.
  - *Rewrite consensus state machines to call the BT directly at every event point* —
    rejected: a large, risky consensus-core rewrite that the reactive limit makes
    unnecessary (the `ControlSignal` read at each interception point achieves the same end).

## D3. Tick source and cadence

- **Decision**: One BT tick per slot advance, driven by the existing `slot_clock.tick()`
  arm in `net-node/src/main.rs`. A slot skip (jump > 1) yields a single tick carrying the
  new slot, matching the existing loop's behaviour.
- **Rationale**: The spec mandates slot-driven ticking, and the node already recomputes
  the slot from wall-clock each tick (`SlotClock::current_slot`), so there is exactly one
  natural tick edge per slot. Reusing it avoids a second timer and keeps the BT in
  lockstep with consensus `on_slot`. The wrapper applies the resulting `ControlSignal` to
  the state machines and publishes the snapshot for the per-peer send actuator.
- **Alternatives considered**: a dedicated BT timer (rejected — drift vs. the slot clock,
  double cadence); ticking per network event (rejected — non-deterministic, not
  slot-aligned, and would re-introduce sub-tick decisions).

## D4. Reactive RUNNING semantics, and the gating house rule

- **Decision**: Composites are **reactive** — every tick re-evaluates from the first child;
  they carry **no** resume cursor. The only state persisted between ticks is a `Join`'s set
  of already-succeeded children and an `Action`'s own progress. When a composite stops
  selecting a child it was running, that child (subtree) is **halted** (an explicit
  abort/reset that, for an `Action`, stops its `ControlSignal` contribution). A `Running`
  result still re-enters the same subtree next tick *as long as the path to it still
  evaluates the same way*. **Gating house rule**: all flow gating lives in explicit
  `Condition` behaviours; a leaf action returns `Running` the whole time it is meant to be
  active and does **not** branch its status on `env`/`state` (the honest fallback returns
  `Success`). Full operational semantics + the `halt` relation:
  [`design/bt-grammar-and-semantics.md`](./design/bt-grammar-and-semantics.md).
- **Rationale**: Reactive (not "memory") is what makes a precondition able to *abort* a
  running action: re-checking the `Condition` each tick lets a `Sequence` fail and halt its
  later, currently-running children when the precondition flips. The memory model (resume at
  the running child, skipping earlier conditions) could not express "act adversarially
  **while** the precondition holds." Confining gating to named `Condition` behaviours keeps
  every flow decision in one readable place. Fully deterministic given the seed and slot
  sequence.
- **Supersedes**: the earlier draft of this decision said "memory" (remember the running
  child's index and resume there); the reactive model above replaces it.
- **Feedback note**: the BT still reacts to the *consequences* of actions, but only by
  sampling `NativeChainState` at the next tick boundary (via Conditions) — never via a
  sub-tick signal.
- **Alternatives considered**: stateless re-evaluation every tick (rejected — can't
  express multi-slot progress); letting leaves also return `Success`/`Failure` from
  tick-time state (valid classic-BT style, rejected as house rule — spreads flow logic
  into leaves).

## D5. Env guarding vs. unguarded chain state

- **Decision**: `DynamicEnv` lives behind `EnvHandle = Arc<std::sync::RwLock<DynamicEnv>>`;
  `NativeChainState` is rebuilt each tick and passed by `&` (unguarded), per the user's
  guidance.
- **Rationale**: Env will be mutated out-of-band once the REST surface lands (D8), so it
  is guarded from the start. `std::sync::RwLock` (not `tokio`) keeps the core sans-IO; an
  async handler can take the write lock briefly without holding it across an `.await`. For
  the MVP, env is set at startup and read by the tick. Chain state is owned and updated
  only at the root tick, so it needs no lock and is cheapest passed by reference.
- **Alternatives considered**: `tokio::sync::RwLock` (rejected — drags async into the
  sans-IO crate); guarding chain state too (rejected — unnecessary contention; the user
  explicitly said it need not be guarded).

## D6. Condition expression language: minimal, hand-rolled

- **Decision**: Support comparisons (`>=`, `>`, `<=`, `<`, `==`, `!=`) over `env.*` /
  `cardano.*` (chain-state) fields against literals or other fields, boolean combinators
  (`and`/`or`/`not`), and a `contains(...)` membership form (as in the spec's
  `cardano.peers.contains(env.target_peer_ip)`). Parse at config-load time; reject
  references to unknown env/state fields then (not at tick time).
- **Rationale**: Confirmed scope (Q3) is "minimal comparisons + membership." A small
  hand-rolled grammar is exhaustively testable, has no dependency cost, and avoids the
  security/validation surface of a general embedded DSL — apt for a red-team tool whose
  configs may be fuzzed next. Conditions are also where *all* gating lives (D4), so a
  clear, total grammar matters.
- **Alternatives considered**: embedding a general expression crate (e.g. `evalexpr`) —
  rejected for MVP per the confirmed decision and the "minimal dependencies" rule; fixed
  named conditions only — rejected as too rigid (the example needs `>=` with a config
  parameter).

## D7. Config split: topology vs. behaviour-tree

- **Decision**: Extract each `[behaviour]` + `[behaviour_selection]` block out of
  `net-cluster/configs/*.toml` into standalone BT configs under
  `net-cluster/behaviours/`. Topology configs keep topology + initial network-shaping
  fields and gain a reference to a BT config by name (plus the selection of which nodes
  run it). The coordinator distributes the referenced BT config to the selected nodes at
  spawn, reusing the existing `BehaviourSelection` resolution.
- **Rationale**: The user asked for this split explicitly; it cleanly separates "the
  network shape" from "what adversaries do," which is also what the upcoming fuzzer needs
  (it fuzzes BT env params, not topology). The existing `BehaviourSelection`
  (`all`/`nodes`/`stake-*`) is reused unchanged.
- **Migration**: `sample-cluster-equivocator.toml`, `-lazy-voter`, `-t22`,
  `-leios-baseline`, and `sample-cluster.toml` each get their `[behaviour]` block moved
  to a named file under `behaviours/`; the topology file references it. Back-compat: an
  inline `[behaviour]` may remain temporarily accepted with a deprecation note, or be
  removed outright — decided in tasks (default: keep a deprecation shim so the net-cluster
  test suite keeps passing during the transition).
- **Distribution transport**: the legacy **stdin** behaviour-swap path
  (`DynamicConfigUpdate.behaviour` / `behaviour_reset`, `set_behaviour`/`reset_behaviour`)
  is **retired** (D2 + D8); for the MVP the BT config reaches each node at spawn via its
  config/overlay, not over stdin.
- **Alternatives considered**: keep everything in one file (rejected — the user's primary
  structural request, and bad for fuzzer ergonomics); a single global behaviour registry
  file (rejected — per-strategy files compose better with `includes`).

## D8. net-node control transport: static config for MVP; REST deferred to Docker

- **Decision**: The MVP control plane is **static config only** (`--config` /
  `--behaviour-tree`). A net-node HTTP/REST control surface (read/replace BT config,
  mutate env) is **deferred** to a later story, and the legacy stdin behaviour-control
  path is **retired** rather than ported.
- **Rationale**: The motivation for REST is the eventual **dockerized** deployment, where
  the coordinator must reach nodes **over the network** — stdin cannot cross container
  boundaries; HTTP can. We are not running in containers yet, so static config suffices
  for the MVP and there is no cross-container need to satisfy now. Hot-swap is
  de-prioritised. When the REST surface lands it will mirror `net-cluster/src/server.rs`
  (axum router, `try_send` to the main loop, `oneshot` tests) and write the guarded
  `EnvHandle` / swap the validated `BehaviourTree` atomically — which is why those are
  designed for it now (D5, and `contracts/net-node-rest.md`).
- **Alternatives considered**: keep stdin as the control transport (rejected — doesn't
  cross containers, and hot-swap is de-prioritised); build REST into the MVP (rejected —
  unnecessary before Docker; adds `axum`/`tokio` server surface the MVP doesn't need).

## D9. Determinism & seed

- **Decision**: The BT root config carries a `seed` in `[metadata]`; all randomized
  action choices derive from it via the crate's `blake2b_simd` helpers
  (`child_seed`/`seed_from_node_id`). No clock reads or `thread_rng` in the engine.
- **Rationale**: Required by spec FR-009/FR-023 (reproducibility, fuzzer prerequisite)
  and by the non-negotiable determinism rule of `shared-consensus`. Because `ControlSignal`
  is plain data, a node can also emit it to telemetry each slot, giving the future fuzzer
  an exact record of what each node was told to do.

## D10. The `ControlSignal` seam: actuator-indexed and domain-grouped

- **Decision**: `ControlSignal` is a plain value type (not a trait), **indexed by actuation
  point** (the one vote decision, the one production strategy, the one per-peer outbound
  transform, the mempool filter) and **grouped into per-domain sub-structs** owned by
  their actuator: `ControlSignal { praos: PraosControl, leios: LeiosControl, mempool:
  MempoolControl }`.
- **Rationale**: Consensus actuation points are *shared, singular resources* — two active
  leaves can target the same one, so the conflict must be reconciled, and that
  reconciliation must happen in the **tick** (the BT-evaluator invariant), handing the actuator
  one resolved value. Keying the seam by *behaviour* would push combination logic back
  into the actuator and re-couple consensus to the behaviour catalogue. Because the seam
  is keyed by *capability*, a new behaviour that reuses an existing actuator changes
  `ControlSignal` **not at all**; only a genuinely new kind of effect (a new actuator)
  extends it — and that already requires touching consensus. Domain sub-structs keep the
  type modular and avoid a single struct everyone edits.
- **Ownership rule**: a *behaviour* owns its config struct (+ its own `Deserialize`), its
  `contribute()`, and its tests, in **one file**; an *actuator* owns its control signal
  sub-struct. Behaviour ⇄ config+logic (encapsulated); actuator ⇄ control signal (shared,
  reconciled in the tick).
- **Alternatives considered**: one flat monolithic `ControlSignal` struct (rejected — merge
  magnet, couples unrelated domains); independent per-behaviour control-signal structs keyed by
  behaviour-prefixed names (rejected — fails when behaviours contend for one resource, and
  moves combination logic into actuators).

## D11. Config form: keyed tables throughout, owner-grouped, explicit `[run].root`

- **Decision**: Author everything as **keyed tables** (no arrays except `children` /
  `includes`): behaviours as `[behaviours.<id>]` (the table key *is* the id), env as
  `[env]` / `[env.<owner>]`, optional module docs as `[metadata.<owner>]`. A module uses
  **one consistent owner word** across its `[metadata.<owner>]`, `[env.<owner>]`, and
  `[behaviours.<owner>...]` (a multi-behaviour module nests as `[behaviours.<owner>.<local>]`). The
  run's identity + entry live in a top-level `[run]` block (`name`, `seed`, `root`).
- **Rationale**: Keyed tables make the *whole document* deep-mergeable (D13) and make
  duplicate ids impossible (TOML rejects duplicate keys). The consistent owner word fixes
  the earlier `env.network_shape` vs `nodes.shape` mismatch. Unordered tables have no
  "first behaviour," so the root is named explicitly in `[run].root` — a feature (explicit, not
  positional).
- **Alternatives considered**: `[[behaviours]]` array-of-tables with `id =` (rejected — arrays
  don't deep-merge; forces bespoke union code and a special rule); owner-first nesting
  `[network_shape.env]` / `[network_shape.behaviours.*]` (rejected for house use — makes `env.*`
  references owner-prefixed and gives shared env an awkward home; section-first keeps
  references uniform while still grouping by owner).

## D12. Parameter overlay via `[env]`, referenced by name, with a precedence ladder

- **Decision**: Overlay-able parameters live in `[env]` / `[env.<owner>]`; behaviours/conditions
  reference them by name (`env.<name>` / `env.<owner>.<name>`), never as baked-in literals.
  "Overlay" = re-declare the key in the closer-to-root config. Precedence (low→high):
  deepest include → shallower includes (list order) → root `[env]` → `--set env.<name>=…`
  CLI → REST runtime write. Layers 1–4 resolve at load into one in-memory env; REST is the
  same ladder applied live.
- **Rationale**: Reference-by-name means one override site changes every reader, and a
  runtime REST update propagates to all referencing behaviours/conditions on the next tick with
  no behaviour patching. The load-time layers are exactly the uniform deep-merge of D13.
- **REST note**: by the time REST calls arrive, parsing/merge are done; REST just writes the
  resolved value behind `EnvHandle` (highest precedence, runtime). Type-mismatched updates
  are rejected, leaving the env unchanged (FR-019).
- **Alternatives considered**: inline literal behaviour params (rejected — can't be overlaid or
  REST-addressed cleanly, and duplicates the value across readers).

## D13. Uniform composition: one deep-merge rule, with `[run]` as the only singleton

- **Decision**: A config plus its `includes` composes by a **single uniform rule** —
  **deep-merge table-by-table, closer-to-root wins** (root over its includes; includes in
  list order). This is `figment`'s deep table merge applied to the whole document; it works
  uniformly because every section is a keyed table (D11). There are **no per-section
  resolution rules**. The only genuinely singular thing is the **run**: `[run]` (`seed`,
  `root`, `name`) deep-merges like everything else, and we *validate* that the resolved
  config has exactly one `[run]` with a `seed` and `root` (a fragment setting `[run]` is a
  lint). A referenced-but-undefined `env.X` is a **hard load-time error**. Env is namespaced
  by owner: top-level `[env]` (or an explicitly-included `shared-env.toml`) for cross-cutting
  params, `[env.<owner>]` for module-local params; promotion to shared is an explicit
  refactor.
- **Rationale**: This is the simplification the earlier "section-aware" design was missing.
  The special-casing of `[metadata]` was an artifact of stuffing the run's singular `seed`/
  `root` into a section; pulling them into a distinct top-level `[run]` makes identity a
  *validation invariant* ("a run has one seed and one entry"), not a merge quirk — so
  composition collapses to one rule that's trivial to reason about and reproduce (key for
  the fuzzer). A same-id behaviour/env key across files simply overlays (root wins), exactly like
  any other key; owner-namespacing is how authors avoid unintended collisions.
- **Alternatives considered**: section-aware resolution with metadata root-only and bespoke
  behaviour-union (rejected — the reviewer correctly flagged the per-section special rules as a
  smell; the uniform rule + `[run]` block supersedes it); treating same-id collisions as a
  load error (rejected — inconsistent with `[env]`'s overlay semantics; namespacing handles
  it); an implicit always-loaded `default_env.toml` (rejected — hidden magic; prefer an
  explicit include for reproducibility).

## D14. Terminology: tree elements are "behaviours"; the registry is the "action registry"

- **Decision**: A behavior-tree element is a **behaviour** (`Behaviour` / `BehaviourKind` /
  `BehaviourId`; TOML `[behaviours.<id>]`); the value it returns when ticked is a
  **`Status`** (Success/Failure/Running). The leaf-action catalogue (formerly
  `BehaviourSpec`) is the **action registry** (`ActionSpec`); its entries are **actions**.
  So: a *behaviour tree* is a tree of *behaviours*; an *Action* behaviour is materialised
  from the *action registry* and, when active, contributes to `ControlSignal`; the consensus
  *actuators* consume `ControlSignal`.
- **Rationale**: "node" was overloaded — net-nodes, topology nodes (the cluster graph uses
  node/edge), and tree elements. "Behaviour" is freed up precisely because the BT evaluator deletes
  the old `Behaviour` *hook trait* (D2), and a "behaviour tree of behaviours" is the
  literal, idiomatic reading (cf. py_trees). Rejected: "node" (the overload), "vertex"
  (graph-flavoured for a tree), "state" (FSM-wrong + collides with `LeiosState`/etc.),
  "step"/"stage" (sequential bias), "phase" (collides with Leios pipeline phases), "task"
  (collides with tokio tasks). The registry is "**action** registry," not "actuator
  registry," because "actuator" already names the consensus-side *consumers* of
  `ControlSignal` — actions *produce*, actuators *consume*.

## D15. Naming: the tick's output is the `ControlSignal` (control-loop framing)

- **Decision**: The value the BT tick produces each slot — formerly "Directives" — is named
  **`ControlSignal`** (domain-grouped: `ControlSignal { praos: PraosControl, leios:
  LeiosControl, mempool: MempoolControl }`). The whole design is a **control loop**, and we
  adopt its standard vocabulary:

  | Control-systems concept | Our system |
  |---|---|
  | Controller | the BT tick |
  | **Control signal** (`u`) — the controller's output | **`ControlSignal`** (the tick's per-slot output) |
  | Actuator | the consensus/networking interception points (already named "actuators") |
  | Plant | Cardano consensus + network (the world being perturbed) |
  | Process variable / feedback (`y`) | `NativeChainState`, sampled each tick |
  | Controller parameters / gains | `env` — and the **fuzzer tunes these**, nothing else |

- **Rationale**: "Directives" never said what it *was*; engineers kept asking what produced
  and consumed it. "Control signal" is the industry-standard term for a controller's output
  to its actuators, and it pairs exactly with the "actuator" name we already use — so the
  whole loop reads coherently: *controller (BT) → control signal → actuators → plant;
  feedback via chain state; `env` = the gains the fuzzer tunes*. It also correctly frames the
  fuzzer (it perturbs the controller's parameters, i.e. `env`, never the output).
- **Rejected**: *control action* (collides with the `Action` behaviour kind), *setpoint* (that
  is the reference/target `r`, not the controller's output — a common mix-up), *manipulated
  variable* (precise process-control term but clunky as a type name), *command* (viable, less
  standard than "control signal").

## D16. BT semantics & node naming

- **Decision**: `Sequence` = ordered AND (stop on first failure); `Selector` = ordered OR
  (stop on first success); the concurrent-AND, fail-fast node is named **`Join`**.
- **Rationale**: AND-`Sequence` is what makes a `Condition` precondition work (a failed
  precondition aborts the rest); an early draft phrased `Sequence` as "run until one
  succeeds," which is `Selector` semantics and incompatible with preconditions — corrected
  to standard AND. `Join` (fork-join: spawn all, wait for all) reads precisely and stays
  distinct from the old parametrised `Parallel`.
- **Rejected names for the concurrent-AND node**: `Parallel` (other BT libraries ship it
  *parametrised* with success/failure thresholds; ours is fixed — mismatched expectations),
  `All` (too terse), `Team`/`Gang` (casual, imprecise). Full semantics live in
  [`design/bt-grammar-and-semantics.md`](./design/bt-grammar-and-semantics.md).

## Open items deferred (not blocking the plan)

- Exact set of MVP "real" demo actions beyond the honest leaf (one of
  `rb-header-equivocator` / `lazy-voter` is the obvious first) — finalized in tasks.
- Whether to keep an inline-`[behaviour]` deprecation shim or hard-cut — finalized in
  tasks; default is a shim to keep the net-cluster suite green during the transition.
- Reconciliation precedence for the rare cases where two active leaves write the same
  `ControlSignal` field (e.g. `Suppress` vs. `Equivocate` on `praos.production`) — default
  is "last active contributor in deterministic traversal order wins," refined per field in
  tasks/data-model as real conflicts appear.
- net-node REST surface details (auth, endpoints) — deferred to the Docker/coordination
  story; sketched in `contracts/net-node-rest.md`.
