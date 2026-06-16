# Contract: net-node Env-Control REST API (DEFERRED — post-MVP / Docker)

> **Status: deferred.** This surface is **not** part of the MVP. We are not running in
> Docker yet, so stdin/static config suffices and there is no cross-container need.
> This contract is the target for when the coordinator must reach nodes **over the
> network** (stdin can't cross container boundaries; HTTP will). It is recorded now so
> the engine's `EnvHandle` and atomic BT-config swap are designed to accommodate it.

A small axum surface on `net-node` for reading and mutating the active BT config and
its `env`, reachable by the `net-cluster` coordinator. Modeled on
`net-rs/net-cluster/src/server.rs` (axum router, `try_send` to the main loop,
`oneshot` tests). Bound to `127.0.0.1:<port>` by default; authorization follows
existing project conventions (the net-cluster server is unauthenticated on loopback).

The MVP control plane is **static config only** (`--config` / `--behaviour-tree`); the
legacy stdin hot-swap path is retired (Model B). The endpoints below land in the
Docker/coordination story.

## Endpoints

| Method | Path                  | Body / Params | Success | Errors |
|--------|-----------------------|---------------|---------|--------|
| GET    | `/api/bt`             | —             | `200` active BT config (name, revision, nodes) + current `env` | `404` if no BT loaded |
| GET    | `/api/bt/env`         | —             | `200` current `DynamicEnv` as JSON | — |
| PUT    | `/api/bt/env/:key`    | `{ "value": <typed> }` | `200` ack; visible to next tick | `400` unknown key / wrong type / out-of-range; current env unchanged |
| POST   | `/api/bt/config`      | full BT config (TOML or JSON) | `200` validated + swapped atomically | `400` validation failure; **prior config stays active** |

## Semantics

- **GET `/api/bt`** (FR-017): returns the effective tree definition and current env
  values.
- **PUT `/api/bt/env/:key`** (FR-018): updates one `DynamicEnv` field under the
  `EnvHandle` write lock (brief, no `.await` held). The new value is read by conditions
  and actions on the **next slot tick** (FR-018, SC-005). Unknown key / type mismatch /
  range violation → `400`, env unchanged (FR-019, US2-4).
- **POST `/api/bt/config`** (FR-018): parse → `validate()` → on success swap the
  `BehaviourTree` atomically (and reset action state cleanly); on failure return `400`
  and keep the running tree (US2-3). Mutations applied in a defined order; the node is
  never left in a partial state (spec Edge Cases).

## Wiring

- The REST handler sends a typed command to the net-node main loop via `try_send`
  (never blocks the loop), exactly like net-cluster's `restart_tx`/`update_tx`/
  `attack_tx` channels. The main loop owns the `EnvHandle` and the `BehaviourTree` and
  applies the change between ticks.
- Federation (US3): the `net-cluster` coordinator calls these per-node endpoints to
  distribute a BT config to a selected set and to set env parameters on a subset,
  reporting per-node success/failure (FR-020..022). This reuses the existing
  `BehaviourSelection` resolution and per-node addressing.

## Testing (TDD)

- axum `oneshot` tests for each endpoint: GET returns the loaded config/env; PUT with a
  valid key updates and acks; PUT with unknown key / wrong type returns `400` and
  leaves env unchanged; POST with a valid config swaps; POST with an invalid config
  returns `400` and the prior config is still active.
- A test asserting an env change is observed by the **next** tick (not the current one).
