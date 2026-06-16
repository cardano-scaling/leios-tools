# Contract: Behavior Tree TOML Config Schema

The on-disk format for a behavior tree, consumed by `BtConfig::load`. Matches the
adversarial example from the spec. This is the stable interface authors and the
coordinator/fuzzer write against.

## Top-level sections

| Section        | Required | Purpose |
|----------------|----------|---------|
| `[metadata]`   | yes      | `name` (string), `revision` (int), `seed` (int, reproducibility) |
| `[env]`        | no       | named parameters; REST/coordinator-mutable; typed (int/float/string/bool) |
| `[[nodes]]`    | yes (≥1) | node definitions; exactly one resolves as the root |
| `includes`     | no       | array of relative paths to sub-behaviour TOMLs (merged, root wins) |

## Node entries (`[[nodes]]`)

Every node has `id` (unique string) and `type`. Additional fields by `type`:

| `type`               | Fields | Returns |
|----------------------|--------|---------|
| `Selector`           | `children = [id, …]` | first child Success → Success |
| `Sequence`           | `children = [id, …]` | all Success → Success; first Failure → Failure |
| `Parallel`           | `children = [id, …]`, `success_policy = "All" \| "Any"` | per policy |
| `Condition`          | `expression = "<expr>"` | Success/Failure |
| `Action` leaves      | `action`-specific fields (see below) | Success/Failure/Running |

### MVP action leaves

Leaf actions are **directive contributors** (Model B): when active they write fields of
the slot's `Directives`; they make no consensus calls. See
[`leaf-action.contract.md`](./leaf-action.contract.md).

| Action `type`        | Fields | Contributes to `Directives` |
|----------------------|--------|-----------------------------|
| `HonestNodeAction`   | `strategy` (informational) | nothing (leaves default = honest) |
| `BehaviourAction`    | `spec = { kind = "...", … }` (a `BehaviourSpec`) | the re-homed leaf's domain fields (e.g. `praos.production`, `praos.outbound`, `leios.vote`, `mempool.tx_filter`) |

`BehaviourAction.spec` is a verbatim `BehaviourSpec` table used as the leaf-kind
discriminant + params — `kind = "rb-header-equivocator"`, `"lazy-voter"`, `"t22"`,
`"deep-reorg"`, or `"drop-inbound-peers"`. (Composition is expressed by the BT structure
itself — `Parallel`/`Sequence` — not a `composite` leaf.) Future action types
(`NetworkShapeAction`, `TxGeneratorAction`) are reserved; the MVP maps the example's
partition/flood leaves onto existing behaviours or stubs them with a logged no-op until
the real catalogue lands.

## Condition expression grammar (minimal)

```
expr     := or
or       := and ("or" and)*
and      := unary ("and" unary)*
unary    := "not" unary | primary
primary  := compare | contains | "(" expr ")"
compare  := value (">="|">"|"<="|"<"|"=="|"!=") value
contains := value ".contains(" value ")"
value    := envref | chainref | int | string
envref   := "env." IDENT
chainref := "cardano." IDENT          # maps to NativeChainState fields
```

All `env.*` names MUST appear in `[env]`; all `cardano.*` names MUST be known
chain-state fields; type mismatches are load-time errors.

## Example (the spec's strategy, adapted to MVP action set)

```toml
[metadata]
name = "Cardano Long-Range Fork & Partition Strategy"
revision = 4
seed = 1234567

[env]
trigger_slot = 345600
trigger_mempool_tx_count = 500

[[nodes]]
id = "root_selector"
type = "Selector"
children = ["sequence_attack_flow", "action_honest_behavior"]

[[nodes]]
id = "sequence_attack_flow"
type = "Sequence"
children = ["cond_slot_reached", "action_adversary"]

[[nodes]]
id = "cond_slot_reached"
type = "Condition"
expression = "cardano.current_slot >= env.trigger_slot"

[[nodes]]
id = "action_adversary"
type = "BehaviourAction"
spec = { kind = "rb-header-equivocator", ways = 2 }

[[nodes]]
id = "action_honest_behavior"
type = "HonestNodeAction"
strategy = "DefaultOuroborosPraos"
```

## Validation outcomes

- Valid config → `Ok(BehaviourTree)`.
- Any violation in data-model.md §"Validation rules" → `Err` naming the offending
  `id`/path/field. The node refuses to start (US1-5); a REST replace is rejected and
  the prior tree stays active (US2-3/4).
