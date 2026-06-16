# Feature Specification: Behavior Tree Engine for Adversarial Nodes

**Feature Branch**: `001-behavior-tree-engine`

**Created**: 2026-06-15

**Status**: Draft

**Input**: User description: "Add a Behavior Tree (BT) engine to net-rs (and possibly sim-rs) that drives adversarial node behavior. Trees are described in TOML, ticked from the node's own slot updates, return SUCCESS/FAILURE/RUNNING, support composite nodes (Selector, Sequence, Parallel), Condition nodes, and Rust-implemented leaf Action nodes ('behaviors'). Configs carry a metadata block (incl. a reproducibility seed), an env parameter block, node definitions, and may include sub-behavior TOML files. A net-node can load a static config from the command line (MVP, top priority); a net-cluster coordinator can distribute and mutate configs and env parameters across a federation of adversarial net-nodes over a REST API. A later fuzzer feature will mutate env parameters and reproduce failure states."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Run a single adversarial node from a static config file (Priority: P1) 🎯 MVP

A red-team engineer authors a TOML file describing a behavior tree (an attack
strategy with an honest-looking fallback), launches a single `net-node` with that
file passed as a command-line argument, and observes the node act on every slot
advance: behaving honestly until the configured trigger conditions are met, then
executing the adversarial actions described by the tree.

**Why this priority**: This is the foundational capability and the stated top
priority. Without a node that can load a tree and tick it from its own slot clock,
no other capability (REST control, federation, fuzzing) has anything to drive. It
delivers standalone value: a single configurable adversarial node usable in
manual experiments against a target.

**Independent Test**: Launch one `net-node` with a known TOML config whose trigger
condition depends on slot height. Observe that before the trigger slot the node
runs the honest fallback, and at/after the trigger slot it runs the adversarial
branch — verifiable from the node's telemetry/logs and the observable effect of the
leaf actions, with no REST API or coordinator involved.

**Acceptance Scenarios**:

1. **Given** a valid TOML config supplied on the command line, **When** the node
   starts, **Then** the tree is parsed, validated, and loaded, and the node reports
   the loaded strategy name and revision.
2. **Given** a loaded tree and a trigger condition `current_slot >= trigger_slot`,
   **When** the node's slot advances past the trigger slot, **Then** the tick that
   follows causes the adversarial branch to execute instead of the honest fallback.
3. **Given** a Selector root with an attack sequence and an honest fallback, **When**
   the attack sequence's conditions are not met, **Then** the honest fallback action
   runs and the root reports SUCCESS.
4. **Given** a slot advance, **When** the engine ticks the tree, **Then** every node
   ticked returns exactly one of SUCCESS, FAILURE, or RUNNING to its parent.
5. **Given** a malformed or self-contradictory config (unknown node type, missing
   child reference, cyclic include), **When** the node starts, **Then** it refuses to
   start and reports a precise, actionable error rather than running partially.

---

### User Story 2 - Inspect and mutate a running node over REST (Priority: P2)

An operator connects to a running `net-node`'s control interface to read the
currently loaded behavior configuration, replace it with a new one, and adjust
individual `env` parameters (e.g., `trigger_slot`, `target_peer_ip`,
`mempool_flood_pps`) while the node continues running, with changes taking effect on
subsequent ticks.

**Why this priority**: Runtime control is what turns a static experiment into a
steerable one and is the prerequisite for coordinator-driven federation (US3) and
the future fuzzer. It builds directly on US1 (a node that already loads and ticks a
tree) and is independently demonstrable against a single node.

**Independent Test**: Start a node with a static config, then use the REST API to (a)
fetch the active config, (b) change an `env` parameter, and (c) confirm via telemetry
that the next tick reflects the new value — all against one node, no coordinator.

**Acceptance Scenarios**:

1. **Given** a running node, **When** the active configuration is requested over REST,
   **Then** the node returns the current tree definition and current `env` values.
2. **Given** a running node, **When** a single `env` parameter is updated over REST,
   **Then** the new value is visible to conditions and actions on the next tick and the
   change is acknowledged.
3. **Given** a running node, **When** a complete replacement configuration is pushed
   over REST, **Then** the node validates it, swaps it in atomically (rejecting it on
   validation failure while keeping the prior config active), and reports the outcome.
4. **Given** an invalid REST mutation (unknown parameter, wrong type, out-of-range),
   **When** it is submitted, **Then** the node rejects it with a descriptive error and
   leaves current behavior unchanged.

---

### User Story 3 - Coordinate a federation of adversarial nodes (Priority: P3)

A `net-cluster` coordinator manages a group of adversarial `net-node`s, distributing
behavior configurations and setting individual `env` parameters across the group so a
coordinated, multi-node strategy (e.g., partitioning a target while flooding its
mempool) can be launched and adjusted from one place.

**Why this priority**: Federation is the scaling step that enables genuinely
coordinated attacks, but it depends on per-node REST control (US2) existing first. It
is valuable on its own once US2 is in place and is demonstrable with a small group of
nodes.

**Independent Test**: Bring up a small federation of nodes via the coordinator,
distribute one config to all of them and a differing `env` parameter to a subset, then
confirm via aggregated telemetry that each node reflects exactly the config and
parameters it was assigned.

**Acceptance Scenarios**:

1. **Given** a federation of running nodes, **When** the coordinator distributes a
   configuration to a named set of nodes, **Then** each targeted node loads it and
   reports success, and the coordinator reports per-node outcomes.
2. **Given** a federation, **When** the coordinator sets an `env` parameter on a subset
   of nodes, **Then** only those nodes reflect the change and the others are unaffected.
3. **Given** a distribution where one node rejects the config, **When** the operation
   completes, **Then** the coordinator surfaces which nodes succeeded and which failed
   without silently masking the failure.

---

### User Story 4 - Compose behaviors from reusable sub-behavior files (Priority: P3)

A red-team engineer factors common sub-trees (e.g., a reusable "honest producer" or a
"mempool flood" payload) into separate TOML files and references them from a root
config via an include mechanism, so strategies can be assembled from shared building
blocks instead of being duplicated.

**Why this priority**: Composition keeps a growing library of attack strategies
maintainable and is important for the fuzzer that follows, but a single self-contained
TOML (US1) already delivers the MVP, so this can follow.

**Independent Test**: Author a root config that includes two sub-behavior files by
relative path, load it into a node, and confirm the resulting effective tree contains
the nodes contributed by each included file and behaves identically to an equivalent
single-file config.

**Acceptance Scenarios**:

1. **Given** a root config with `includes` referencing other TOML files by relative
   path, **When** it is loaded, **Then** the included sub-behaviors are resolved and
   merged into one effective tree.
2. **Given** an include that cannot be resolved or introduces a cycle, **When** the
   config is loaded, **Then** loading fails with a clear error identifying the offending
   include.
3. **Given** included files that define `env` parameters, **When** they are merged,
   **Then** parameter precedence is well-defined and reported (root overrides includes).

---

### Edge Cases

- A tick arrives before the previous tick's RUNNING actions have completed — the engine
  MUST define and apply consistent semantics (resume vs. restart) for in-progress nodes.
- The chain skips slots (jumps forward by more than one) — the engine MUST still produce
  a well-defined tick rather than ticking once per skipped slot or losing the advance.
- A leaf action references an `env` parameter that is absent or of the wrong type — this
  MUST be caught at load/validation time, not silently at tick time.
- A Condition expression references a state or env field that does not exist — MUST fail
  validation rather than evaluate to an arbitrary default.
- Two REST mutations arrive close together (config replace + parameter set) — the node
  MUST apply them in a defined order without leaving the tree in a partial state.
- An adversarial config is reloaded mid-run — the node MUST not crash or leave dangling
  effects from leaf actions started under the previous tree.
- The same root config run twice with the same seed MUST produce the same sequence of
  randomized decisions (reproducibility), and a different seed MUST be able to diverge.

## Requirements *(mandatory)*

### Functional Requirements

#### Behavior tree model & execution

- **FR-001**: The system MUST execute a behavior tree by ticking from the root toward
  the leaves, where each ticked node returns exactly one status: SUCCESS, FAILURE, or
  RUNNING.
- **FR-002**: A tick MUST be driven by the node's own slot progression: when the node
  detects the chain has advanced to a new slot, exactly one tick MUST be delivered to
  the root for that advance.
- **FR-003**: The system MUST support composite node types: a Selector (succeeds when
  any child succeeds, tries children in order), a Sequence (succeeds only when all
  children succeed in order), and a Parallel (ticks multiple children with a
  configurable success policy, e.g., "All").
- **FR-004**: The system MUST support Condition nodes that evaluate a boolean expression
  over available `env` parameters and node `state` and return SUCCESS or FAILURE.
- **FR-005**: The system MUST support leaf Action nodes ("behaviors") that are
  implemented in code by engineers and registered by a type name referenced from the
  TOML (e.g., a network-shaping action, a transaction-generator action, an honest-node
  action).
- **FR-006**: A node returning RUNNING MUST be re-entered on subsequent ticks according
  to documented, consistent semantics so that multi-tick actions can make progress.

#### Configuration format

- **FR-007**: Behavior trees MUST be described entirely in TOML configuration files; the
  tree structure MUST NOT require code changes to define a new strategy from existing
  node and behavior types.
- **FR-008**: A configuration MUST contain a metadata block identifying at least the
  strategy name and a revision.
- **FR-009**: A root configuration MUST contain a reproducibility seed so that any
  randomized decisions made during execution are deterministic and reproducible for a
  given seed (a prerequisite for the later fuzzer feature).
- **FR-010**: A configuration MUST contain an `env` block of named parameters that are
  readable by Condition and Action nodes and writable by both configuration files and
  the REST API.
- **FR-011**: Node `state` (e.g., current slot, current epoch, mempool transaction
  count, connected peers) MUST be readable by Condition and Action nodes.
- **FR-012**: Configurations MUST support including other configuration files by relative
  path to compose reusable sub-behaviors, with well-defined merge and parameter-
  precedence rules and cycle detection.
- **FR-013**: The system MUST validate a configuration before activating it, rejecting
  unknown node types, dangling child/include references, cycles, and references to
  missing or mistyped `env`/`state` fields, with precise error messages.

#### Single-node operation (MVP)

- **FR-014**: A `net-node` MUST accept a path to a behavior configuration as a
  command-line argument and run that configuration as a static strategy.
- **FR-015**: A node MUST expose, via telemetry/logs, enough information to observe which
  branch/leaf executed on a given tick and the status each node returned, sufficient to
  verify behavior in tests and experiments.
- **FR-016**: When no behavior configuration is supplied, a node MUST retain its current
  default (honest) operation and MUST NOT require a behavior tree to function.

#### Runtime control (REST)

- **FR-017**: A `net-node` MUST expose a control interface that allows an authorized
  caller to read the currently active configuration and current `env` values.
- **FR-018**: The control interface MUST allow replacing the active configuration and
  setting individual `env` parameters at runtime, with validation, atomic application,
  and acknowledgement of success or failure.
- **FR-019**: Rejected control operations MUST leave the node's current behavior
  unchanged.

#### Federation (coordinator)

- **FR-020**: The `net-cluster` coordinator MUST be able to distribute a configuration to
  a selected set of adversarial nodes and report per-node success/failure.
- **FR-021**: The coordinator MUST be able to set individual `env` parameters on selected
  nodes without affecting unselected nodes.
- **FR-022**: The coordinator MUST surface partial failures (some nodes accept, others
  reject) rather than masking them.

#### Quality & reproducibility

- **FR-023**: Given identical configuration and seed, two runs MUST produce the same
  sequence of randomized decisions (deterministic reproducibility).
- **FR-024**: All behavior MUST be covered by automated tests per the project
  constitution's Test-Driven Development principle, or by a recorded, human-confirmed
  manual test where automation is impractical.

### Key Entities

- **Behavior Configuration**: A TOML document defining a strategy — metadata (name,
  revision), a seed, an `env` parameter block, the set of nodes, and optional includes.
- **Behavior Tree**: The effective tree assembled from a root configuration plus any
  included sub-behaviors; rooted at a single node; ticked as a unit.
- **Node**: A tree element with a type and an id. Composite nodes (Selector, Sequence,
  Parallel) reference children by id; Condition nodes hold an expression; Action nodes
  ("behaviors") reference a registered behavior type and its parameters.
- **Node Status**: The result of ticking a node — SUCCESS, FAILURE, or RUNNING.
- **Env (Dynamic Parameters)**: Named, externally mutable values (via config or REST)
  read by conditions and actions, e.g., trigger slot, target peer, flood rate, shaping
  delay/drop.
- **Node State (Native Chain State)**: Read-only-to-the-tree metrics owned by the node,
  e.g., current slot, current epoch, mempool transaction count, connected peers.
- **Behavior (Leaf Action)**: A code-implemented unit registered under a type name that
  performs an adversarial or honest effect when ticked and reports a status.
- **Coordinator**: The federation controller that distributes configurations and
  parameters across a group of adversarial nodes and aggregates their outcomes.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A red-team engineer can take a target node from honest behavior to a
  configured adversarial behavior solely by editing a TOML file and restarting the node —
  with zero code changes — for any strategy built from existing node and behavior types.
- **SC-002**: For a config whose attack triggers at a target slot, 100% of runs switch
  from honest to adversarial behavior on the first tick at or after that slot, and never
  before it.
- **SC-003**: Two runs of the same configuration and seed produce identical sequences of
  randomized decisions in 100% of trials.
- **SC-004**: Every invalid configuration in a defined suite of malformed inputs is
  rejected at load time with an actionable error, and 0% of malformed configs result in a
  partially-running or silently-degraded tree.
- **SC-005**: An operator can change a running node's behavior parameter and see it take
  effect on the next slot tick, without restarting the node.
- **SC-006**: A coordinator can apply a configuration and per-node parameters across a
  federation such that each node reflects exactly its assigned configuration and
  parameters, with any rejection visibly reported.
- **SC-007**: A newly added leaf behavior (written by an engineer) can be referenced from
  TOML and exercised without modifying the engine's core tick/traversal logic.

## Assumptions

- **Placement (confirmed)**: The engine core lives in a shared crate (under `shared-rs`)
  and is wired into `net-rs` (`net-node` and `net-cluster`) in this feature. The API is
  designed to be reusable by `sim-rs` later, but no `sim-rs` integration is delivered
  here.
- **MVP behavior catalog (confirmed)**: The MVP (US1) delivers the engine, the TOML
  format, slot-driven ticking, the composite and condition node types, and a small set of
  functional leaf behaviors — an `HonestNodeAction` plus one or two simple, real
  adversarial actions (e.g., a basic transaction generator and/or a logging network-shape
  action) — sufficient to demonstrate an honest-vs-adversarial switch end-to-end. The full
  catalog of production-grade adversarial behaviors (real packet shaping, invalid-Plutus
  flooding, etc.) is delivered incrementally in later work.
- **Condition expression language (confirmed)**: Conditions support comparisons (`>=`,
  `==`, etc.), boolean combinations (and/or/not) over `env`/`state` fields, and simple
  membership checks (as in the example `peers.contains(...)`). A fuller general-purpose
  expression DSL is explicitly out of scope for this feature.
- The REST control interface reuses the existing `net-cluster`/`net-node` control
  conventions already present in the codebase rather than introducing a new transport.
- Authorization for the control/REST interface follows existing project conventions;
  this tooling is for authorized adversarial testing only.
- The slot/tick source is the node's existing slot clock; the engine consumes slot
  advances rather than maintaining its own timer.

## Dependencies

- Builds on the existing `net-node` slot clock, mempool, and networking components as the
  source of node `state` and the surfaces leaf behaviors act upon.
- Builds on the existing `net-cluster` coordinator and its node-control/process
  management for federation (US3).
- The planned fuzzer feature depends on FR-009/FR-023 (seeded reproducibility) and on the
  REST parameter-mutation surface (US2); this feature must not preclude it.
