# Contract: Behavior Tree TOML Config Schema

The on-disk format for a behavior tree, consumed by `BtConfig::load`. This is the stable
interface authors, the coordinator, and the fuzzer write against.

## Top-level sections

The document is **all nested tables**, composed by a single uniform rule (see
Composition). The sections:

| Section            | Required | Purpose |
|--------------------|----------|---------|
| `[run]`            | yes (root) | the run's identity + entry: `name` (string), `seed` (int, reproducibility), `root` (id of the root behaviour). Exactly one resolved `[run]`; supplied by the root config. |
| `includes`         | no       | top-level array of relative paths to sub-behaviour TOMLs (any file may include) |
| `[metadata.<owner>]` | no     | optional per-module documentation (`revision`, `description`, …) |
| `[env]` / `[env.<owner>]` | no | named parameters; overlay-able and (later) REST-mutable; typed (int/float/string/bool). Top-level keys are shared/cross-cutting; `[env.<owner>]` are owner-namespaced |
| `[behaviours.<id>]`     | yes (≥1) | behaviour definitions, keyed by id (the table key **is** the id); a multi-behaviour module nests as `[behaviours.<owner>.<local>]` |

`run.root` names the root behaviour explicitly (keyed behaviour tables are unordered, so there is
no "first behaviour" to imply it). `[run]` is supplied by the root config; a fragment that
sets it is flagged (see Composition). The owner word is used **consistently** across a
module's `[metadata.<owner>]`, `[env.<owner>]`, and `[behaviours.<owner>...]`.

## Behaviour entries (`[behaviours.<id>]`)

The id is the table key; there is no `id =` field, and duplicate ids are impossible
(TOML rejects duplicate keys). A module that owns several behaviours nests them under its owner
(`[behaviours.<owner>.<local>]`), giving dotted ids like `network_shape.shape` referenced in
`children`. Fields by `type`:

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
[`leaf-action.contract.md`](./leaf-action.contract.md). Per the gating house rule, a leaf
returns `Running` while active; flow gating lives in `Condition` behaviours.

| Action `type`        | Fields | Contributes to `Directives` |
|----------------------|--------|-----------------------------|
| `HonestAction`   | `strategy` (informational) | nothing (leaves default = honest); returns `Success` |
| `Action`    | `spec = { kind = "...", … }` (an `ActionSpec`) | the re-homed leaf's domain fields (e.g. `praos.production`, `praos.outbound`, `leios.vote`, `mempool.tx_filter`) |

`Action.spec` is a verbatim `ActionSpec` table used as the action-kind
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
envref   := "env." DOTTED_IDENT       # e.g. env.trigger_slot, env.network_shape.packet_delay
chainref := "cardano." IDENT          # maps to NativeChainState fields
```

All `env.*` names MUST resolve in the merged `[env]` (a referenced-but-undefined env key
is a hard load-time error); all `cardano.*` names MUST be known chain-state fields; type
mismatches are load-time errors.

## Composition & namespacing

Composition is a **single uniform rule**: a config plus its `includes` is **deep-merged
table-by-table, with the closer-to-root file winning** on any key conflict (the root
config wins over what it includes; includes apply in list order). This is exactly
`figment`'s deep table merge — and it works for the *whole* document because every section
is a keyed table (no arrays to special-case except `children`/`includes`, which are
defined in one place and replace-on-conflict). There are **no per-section resolution
rules**.

The only genuinely singular thing is the **run**: a strategy has exactly one `seed` and
one `root` entry behaviour. Those live in `[run]`, which deep-merges like everything else; we
simply **validate** that the resolved config has exactly one `[run]` carrying a `seed` and
a `root`, and flag a fragment that sets `[run]` (a lint — the root owns the run). That's a
validation invariant ("a run has one seed and one entry"), not a special merge rule. A
root-owned `seed` also guarantees every included module shares one deterministic seed.

### Env ownership tiers

To keep "where a parameter is defined" meaningful rather than arbitrary:

- **Top-level `[env]`** (or a dedicated `shared-env.toml` the root explicitly includes) —
  genuinely cross-cutting params, referenced as `env.<name>`.
- **`[env.<owner>]`** — params a single module owns, referenced as `env.<owner>.<name>`.
  Namespaced, so they neither clutter nor collide; the owner word matches the module's
  `[metadata.<owner>]` / `[behaviours.<owner>...]`.
- **Promotion is the explicit refactor**: when a second consumer appears, move the
  declaration from `[env.<owner>]` up to shared and update the (greppable) references.
- Keep shared env an **explicit** include (no implicit always-loaded file) so resolution
  stays transparent and reproducible for the fuzzer.

A referenced-but-undefined `env.X` is a **hard load-time error** — the backstop that makes
any organization choice safe.

### Parameter overlay & precedence

Overlay-able parameters live in `[env]`; behaviours/conditions **reference them by name** (not
baked-in literals), so one override site changes every reader. Precedence, lowest to
highest:

1. deepest included file's `[env]`
2. shallower includes, in `includes` list order
3. the **root** config's `[env]`
4. `--set env.<name>=<value>` on the CLI (existing net-node/net-cluster mechanism)
5. **REST** `PUT /api/bt/env/<name>` at runtime (deferred; post-MVP/Docker)

Layers 1–4 resolve at load time into one in-memory env; layer 5 writes that same resolved
value live (highest precedence). Because references are by name, a runtime update is seen
by every referencing behaviour/condition on the next tick.

## Worked example (canonical)

Three files: a shared-env file, a reusable behaviour module, and the root strategy that
includes both and overlays a parameter. Note the consistent owner word `network_shape`
across the module's `[metadata.*]`, `[env.*]`, and `[behaviours.*]`, and the single uniform
deep-merge (root wins).

`behaviours/shared-env.toml` — cross-cutting params, nothing else:
```toml
[env]
trigger_slot = 345600          # gates the whole attack; read by a Condition
```

`behaviours/network-shape.toml` — a reusable module that owns its own param + behaviour:
```toml
[metadata.network_shape]       # optional module docs
revision = 1
# NB: no [run] — only the root config carries seed/root.

[env.network_shape]            # owner-namespaced param
packet_delay = 20              # default; the includer may overlay it

[behaviours.network_shape]          # the module behaviour (same owner word)
type = "Action"
spec = { kind = "drop-inbound-peers", probability = 1.0 }
# (placeholder leaf; a real NetworkShapeAction reads env.network_shape.packet_delay later)
```

`behaviours/long-range-fork.toml` — the root strategy:
```toml
[run]
name = "Cardano Long-Range Fork & Partition Strategy"
seed = 1234567                 # the one reproducibility seed for the run
root = "root_selector"         # the one entry behaviour
includes = ["shared-env.toml", "network-shape.toml"]

[env.network_shape]
packet_delay = 10              # OVERLAY: deep-merge, root wins (20 -> 10)

# Root Selector: try the attack; otherwise behave honestly.
[behaviours.root_selector]
type = "Selector"
children = ["attack_flow", "honest"]

# Attack branch: only once we pass the trigger slot.
[behaviours.attack_flow]
type = "Sequence"
children = ["cond_slot_reached", "exploit"]

[behaviours.cond_slot_reached]
type = "Condition"
expression = "cardano.current_slot >= env.trigger_slot"

# Run partition + equivocation concurrently while active.
[behaviours.exploit]
type = "Parallel"
success_policy = "All"
children = ["network_shape", "equivocate"]   # `network_shape` comes from the module

[behaviours.equivocate]
type = "Action"
spec = { kind = "rb-header-equivocator", ways = 2 }

# Honest fallback.
[behaviours.honest]
type = "HonestAction"
strategy = "DefaultOuroborosPraos"
```

Resolved result (one deep-merge, root wins):
- exactly one `[run]` — from `long-range-fork.toml` (`seed = 1234567`, `root = "root_selector"`);
  the fragments carry none.
- `[env]` = `{ trigger_slot = 345600 (shared), network_shape.packet_delay = 10 (overlaid) }`.
- `[behaviours.*]` = `{ root_selector, attack_flow, cond_slot_reached, exploit, equivocate,
  honest }` (root) merged with `{ network_shape }` (module).
- Before slot 345600: `cond_slot_reached` → Failure → `attack_flow` Failure → `Selector`
  falls through to `honest`. At/after 345600: `exploit` runs `network_shape` + `equivocate`.

## Validation outcomes

- Valid config → `Ok(BehaviourTree)`.
- Any violation in data-model.md §"Validation rules" → `Err` naming the offending
  `id`/path/field (unknown type, dangling child/include, cycle, **undefined `env.X`
  reference**, mistyped condition ref, missing/duplicate `[run]`, or `[run]` set in an
  included fragment). The node refuses to start (US1-5); a REST replace is rejected and
  the prior tree stays active (US2-3/4).
- A same-id behaviour or env key appearing in more than one file is **not** an error — it
  deep-merges with the closer-to-root file winning (the uniform overlay rule), exactly as
  for `[env]`. Authors namespace by owner (`[behaviours.<owner>...]`, `[env.<owner>]`) to avoid
  unintended collisions.
