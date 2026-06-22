# Quickstart / Validation Guide: Behavior Tree Engine

Runnable scenarios that prove the feature works end-to-end. Implementation details
live in `data-model.md`, `contracts/`, and the eventual `tasks.md`; this is the
validation/run guide. Commands are run from `net-rs/` unless noted. Per the project
constitution, automated tests are the primary evidence; the live cluster scenario is a
human-confirmed manual test (Principle II) — record its output.

## Prerequisites

```sh
cd net-rs && cargo build -p net-node -p net-cluster
```

## Scenario 1 — Engine core unit tests (US1, primary evidence)

Pure, fast, deterministic. Covers tick/status, composites, conditions, validation,
and seed reproducibility.

```sh
cd shared-rs && cargo test -p shared-consensus behaviour::tree
```

Expected: tests pass, including —
- a Selector root runs the honest fallback when the attack Sequence's condition is
  false, and switches to the attack branch once `cardano.current_slot >= env.trigger_slot`
  (SC-002);
- every ticked behaviour returns exactly one `Status` (FR-001);
- malformed configs (unknown behaviour type, dangling child, cyclic include, mistyped env
  ref) are rejected at load with a precise error (SC-004);
- two ticks of the same config + seed produce identical effect sequences (SC-003).

## Scenario 2 — Single node from a static BT config (US1 MVP, top priority)

Run one node with a BT config whose trigger depends on slot height; observe the
honest→adversarial switch in telemetry.

```sh
# fast slots so the trigger is reached quickly
cargo run -p net-node -- \
  --config net-node/configs/mainnet.toml \
  --config net-node/configs/node0.toml \
  --behaviour-tree net-cluster/behaviours/slot-trigger-equivocator.toml \
  --set slot_duration_ms=200 --set behaviour_tree.env.trigger_slot=50
```

Expected (from `node0-events.jsonl` / logs):
- before slot 50: honest behaviour, no equivocation events;
- at the first tick with slot ≥ 50: the BT activates the configured `ActionSpec`
  and adversarial events appear (e.g. duplicate `RBGenerated` for the equivocator);
- the loaded strategy name + revision are logged at startup (US1-1).

## Scenario 3 — Config split round-trips (US4 / D7)

The topology config carries no `[behaviour]` block; the BT config lives under
`behaviours/` and is referenced by name. Loading the cluster still works.

```sh
# topology-only config + referenced behaviour file
cargo run -p net-cluster -- \
  --config net-cluster/configs/sample-cluster-equivocator.toml \
  --net-node-bin target/debug/net-node
```

Expected: cluster starts; the selected node(s) run the BT from
`net-cluster/behaviours/…`; the topology file contains only topology + initial
network-shaping fields. A `cargo test -p net-cluster` config test asserts the split
loads and that an inline `[behaviour]` (if the deprecation shim is kept) still works
with a warning.

## Scenario 4 — Runtime env mutation over REST (DEFERRED — post-MVP / Docker)

> Not part of the MVP (no Docker yet; static config suffices). Recorded for the
> Docker/coordination story. See `contracts/net-node-rest.md`.

With a node's REST surface enabled, change a trigger parameter and see it take effect
on the next tick without restart.

```sh
# assuming the node exposes its control port (see net-node-rest.md)
curl -s localhost:<port>/api/bt/env | jq .            # read current env
curl -s -X PUT localhost:<port>/api/bt/env/trigger_slot \
     -H 'content-type: application/json' -d '{"value": 10}'   # 200 ack
```

Expected: `200` ack; the next slot tick evaluates conditions with `trigger_slot = 10`
(SC-005). An invalid key/type returns `400` and leaves behaviour unchanged (US2-4).
Primary evidence is the axum `oneshot` test suite for `contracts/net-node-rest.md`.

## Scenario 5 — Coordinated federation (US3 / P3, manual + recorded)

Bring up a small cluster, distribute a BT config to a subset via the coordinator, set
an env parameter on part of the subset, and confirm via aggregated telemetry that each
node reflects exactly its assignment, with any rejection reported (FR-020..022, SC-006).

```sh
cargo run -p net-cluster -- --config net-cluster/configs/sample-cluster.toml \
  --net-node-bin target/debug/net-node
# then drive distribution via the coordinator API / existing attack-trigger path
scripts/cluster-status.sh
```

This is a human-confirmed manual test: record the per-node outcomes and the aggregated
status (Principle II).

## Gate before commit (constitution Principle V)

For each affected workspace (`shared-rs`, `net-rs`):

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

All three must pass (constitution Principle I + V). `shared-rs` writes from a net-rs
worktree need `dangerouslyDisableSandbox` per shared-consensus CLAUDE.md.
