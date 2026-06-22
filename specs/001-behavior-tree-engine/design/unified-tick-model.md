# Architecture: single slot-tick BT producing a `ControlSignal`

Rationale and rejected alternatives are in [`../research.md`](../research.md) (D2, D10, D13, D15).

Adversarial behaviour is a **control loop**:

| Role | Our system |
|------|------------|
| Controller | the behaviour tree, ticked once per slot |
| Control signal (`u`) | `ControlSignal` — the tick's per-slot output (types: [`../data-model.md`](../data-model.md)) |
| Actuators | the consensus/networking interception points that read `ControlSignal` and apply it |
| Plant | Cardano consensus + network |
| Feedback (`y`) | `NativeChainState`, sampled each tick |
| Gains | `env` — the fuzzer tunes only these |

## Decision vs. actuation

All decisions happen in the tick; the BT structure is the **only** locus of control.
Actuation is mechanical: Cardano consensus is reactive — EBs, votes, txs, and blocks arrive
*between* ticks — so some effects must be applied at event-time interception points. Those
points only **read** the `ControlSignal` the last tick produced; they never decide.

`ControlSignal` is domain-grouped by actuator (`praos`/`leios`/`mempool`); each active leaf
action writes its slice, and conflicts on the same field are reconciled in the tick. The
actuator reads one resolved value.

## Replaces / keeps

**Deletes**: the `Behaviour` hook trait, `BehaviourOutcome`/`DecisionOutcome`,
`CompositeBehaviour`, and the `invoke_hook` plumbing in `leios.rs`/`praos.rs`/`mempool.rs`.

**Keeps**: the registry (`ActionSpec` + `build`) as the action lookup; the shipped attack
mechanics (equivocation routing, reorg, inbound reset, vote abstention, T22 filtering),
re-homed as control-signal contributors; determinism (seed threaded; sans-IO;
`BTreeMap`/`BTreeSet`).

## Actuators to convert

Each site formerly called a hook; each now reads a `ControlSignal` field:

| Site | Reads | Cadence |
|------|-------|---------|
| net-node `main.rs` slot arm | `praos.reorg_depth`, `praos.drop_inbound` | slot |
| `production.rs` | `praos.production`, `praos.body_path` | slot (at production) |
| net-core `server_handlers.rs` — RB-header send | `praos.outbound` | event |
| net-core `server_handlers.rs` — `serve_leios_notify` offer send | `leios.offer_eb_size`, `leios.echo_to_source` | event |
| `leios.rs` vote path | `leios.vote` | per EB |
| `mempool.rs` | `mempool.tx_filter` | event |

The tick is driven from the existing `slot_clock.tick()` arm in net-node `main.rs`: build
`NativeChainState`, tick the BT, apply + publish the `ControlSignal`. The `serve_leios_notify`
actuator needs the per-offer `source`/`eb_size` fields on `NotificationEntry` (merged with the
`lie-about-eb-size`/`echo-to-source` work); the rewire reads `ControlSignal.leios` instead of
calling the old `allow_echo_to_source`/`transform_outbound` hooks.

## Conventions

- **Gating**: all flow gating lives in `Condition` behaviours; leaves return `Running`
  while active. Full semantics: [`bt-grammar-and-semantics.md`](./bt-grammar-and-semantics.md).
- **Config composition** (uniform deep-merge, `[run]` singleton, owner-namespaced env):
  research D11–D13.
- **Control plane**: static config for the MVP; net-node REST is deferred
  (post-MVP/Docker); the stdin hot-swap path is retired.
