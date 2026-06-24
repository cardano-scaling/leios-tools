# `behaviours/` — adversarial behaviour trees

This directory is the **authoring home** for the behaviour trees (BTs) that drive
adversarial / experimental Cardano-Leios nodes. You write a tree in a small
`.bt` surface language; the `bt.py` translator turns it into the self-contained
TOML the engine loads; `make configs` generates the configs that `net-node`,
`net-cluster`, and `sim-rs` consume.

The BT **engine** lives in `shared-rs/consensus/src/behaviour/tree/` (sans-IO,
deterministic) and is shared by all three workspaces. The authoritative model is
in the spec:

- Grammar & operational semantics — [`../specs/001-behavior-tree-engine/design/bt-grammar-and-semantics.md`](../specs/001-behavior-tree-engine/design/bt-grammar-and-semantics.md)
- TOML config contract — [`../specs/001-behavior-tree-engine/contracts/bt-config.schema.md`](../specs/001-behavior-tree-engine/contracts/bt-config.schema.md)
- Leaf-action (Rust) contract — [`../specs/001-behavior-tree-engine/contracts/leaf-action.contract.md`](../specs/001-behavior-tree-engine/contracts/leaf-action.contract.md)

## Behaviour trees quickstart

The tree is **ticked once per slot**. Each node, when ticked, returns
`Success` / `Failure` / `Running`. Evaluation is **reactive**: every tick
re-evaluates from the first child, so a `Condition` precondition is re-checked
each slot and can abort (`halt`) a running subtree. The tick is the *only* place
adversarial decisions are made — it accumulates a typed **`ControlSignal`**, and
the consensus actuators (voting, RB production, EB processing, per-peer sends)
read that signal mechanically. "Honest" is simply the default `ControlSignal`.

| Kind | Form | Meaning |
|---|---|---|
| `Sequence` | `Sequence[ a, b, … ]` | ordered **AND** — fail on first `Failure`; all `Success` ⇒ `Success` |
| `Selector` | `Selector[ a, b, … ]` | ordered **OR** / fallback — succeed on first `Success`; all fail ⇒ `Failure` |
| `Join` | `Join[ a, b, … ]` | concurrent **AND**, fail-fast — tick all pending children each tick |
| `ForTicks` | `ForTicks(n, child)` | run `child` for at most `n` ticks (slots), then halt it and return `Success` |
| `Condition` | `Condition(expr)` | evaluate a predicate over env/chain state; immediate `Success`/`Failure` |
| `Action` | `Action("kind", …)` | a leaf that contributes to `ControlSignal`; `Action("honest")` is the no-op |

## Directory layout

- **`lib/` — base behaviours.** Pure fragments: **no `run`, no `root`**, just
  *named* behaviours (a single leaf with its default parameters, or a reusable
  composite). These are the building blocks; they are never loaded directly.
- **`attacks/` — runnable configs.** Each supplies `run { name, seed }`, optional
  `env`, and `root [...]`, and either `include`s `lib/` behaviours and references
  them by name (optionally overriding parameters) or inlines simple behaviours.
  **These are what `make configs` resolves into the consumer TOML** — only they
  carry the `[run]` block the engine requires.

`honest` is the one non-adversarial base behaviour. The specific adversaries are
intentionally **not catalogued here** — see the files in `lib/` and `attacks/`
(and `ActionSpec` in `shared-rs/consensus/src/behaviour/registry.rs` for the set
of available action `kind`s).

## The `.bt` surface language

```
# Comments start with '#'.

include [ "some-behaviour.bt" ]   # pull in lib/ behaviours (resolved by --resolve)

run {                              # required for a runnable attack (omitted in a lib/ behaviour)
  name = "my-attack"
  seed = 1234567                   # the one reproducibility seed
}

env {                             # optional: parameters referenced by conditions
  trigger_slot = 345600
}

# A gated strategy: honest until the trigger slot, then run a behaviour for 3 slots.
Selector "strategy" [
  Sequence[
    Condition(cardano.current_slot >= env.trigger_slot),
    ForTicks(3, "some-behaviour")    # reference a lib/ behaviour by name
  ],
  Action("honest")                   # fall back to honest
]

root [ "strategy" ]               # required for a runnable attack: the entry behaviour
```

- **Children** are either a bare-name **reference** (`"strategy"`,
  `"some-behaviour"`) to a named behaviour, or an **inline** behaviour.
  References *expand* to independent instances (each gets its own node-local
  state).
- **Reference with parameter overrides:** a reference may override a subset of
  the referenced behaviour's action parameters — the rest keep that behaviour's
  defaults (a merge). Overrides are applied at `bt.py --resolve` time:
  ```
  root [ "some-behaviour"(threshold = 50) ]   # inherit defaults, override `threshold`
  ```
- **Condition grammar:** comparisons (`>= > <= < == !=`), `and` / `or` / `not`,
  and `value.contains(value)`. Values are `env.<dotted.name>`,
  `cardano.<field>` (`current_slot`, `current_epoch`, `mempool_tx_count`),
  integers, or `"strings"`. Every `env.*` / `cardano.*` reference is validated
  (and type-checked) at load time.
- **`Action("honest")`** is the dedicated no-op leaf (resolves to the engine's
  `type = "HonestAction"`).

## The translator (`bt.py`)

Pure-Python, stdlib only (needs Python ≥ 3.11 for `tomllib`). Reads one file
(or `-` for stdin); always writes to **stdout**. Direction is inferred from the
extension unless forced.

```sh
./bt.py file.bt                       # .bt  -> TOML   (direction inferred)
./bt.py file.toml                     # TOML -> .bt    (direction inferred)
./bt.py --bt-to-toml -                # force .bt  -> TOML (stdin -> stdout)
./bt.py --toml-to-bt -                # force TOML -> .bt
./bt.py --resolve --include-path lib attacks/x.bt   # expand -> one self-contained TOML
```

`--resolve` is the **build step**: it follows `include`s by name (searching the
`--include-path` dirs first, library-first), deep-merges into one config,
**applies parameter overrides** (specialising each overridden reference into a
concrete behaviour), drops behaviours unreachable from `root`, and validates that
every reference resolves. The engine only loads self-contained configs — it
**rejects** a config that still carries `includes`.

## The build process (`make`)

The runnable **`attacks/*.bt`** files are the **single source of truth** for the
configs the node and simulator load. The source → destination mapping lives in
the `Makefile` (`CONFIG_MAP`); each `attacks/<x>.bt` resolves to a
`net-cluster/behaviours/*.toml` (and the simulator's `parameters/behaviours/`
where applicable). Each generated file carries a `# GENERATED …` header.

| `make …` | Does |
|---|---|
| `configs` | regenerate the consumer configs from their `attacks/*.bt` sources |
| `check-configs` | regenerate to a temp and `diff` against the committed configs — fails on drift |
| `test` | `.bt` ⇄ TOML round-trip idempotence over `lib/` + `attacks/` |
| `test-resolve` | compare `--resolve` of the sample attack against the committed golden (`expected/`) |
| `bless-resolve` | regenerate that golden (after an intentional change) |
| `check` | `test` + `test-resolve` + `check-configs` (this is what CI runs) |

CI: [`../.github/workflows/behaviours.yaml`](../.github/workflows/behaviours.yaml)
runs `make check`, so an `attacks/`/`lib/` edit without re-running `make configs`
fails the build.

## How consumers select a behaviour tree

- **net-node:** `--behaviour-tree <path-to-resolved.toml>` (or the
  `behaviour_tree` config key). Absent ⇒ an implicit honest node (no tree).
- **net-cluster:** `behaviour_tree = "<path>"` + a `[behaviour_selection]` block
  (`all` / `nodes` / `stake-ordered` / `stake-random` / `stake-fraction`)
  choosing which nodes get it installed at spawn. Unselected nodes stay honest.
- **sim-rs:** under `leios-variant: shared-consensus`, a `consensus-behaviours`
  entry pairs `behaviour-tree: <path>` with a `selection`.

## Authoring guide

### A new base behaviour (`lib/`)

Add `lib/<name>.bt` as a **pure fragment** — a named behaviour with its default
parameters, **no `run`/`root`**:

```
# lib/<name>.bt
Action "<name>" ("<action-kind>", param = default, …)
```

### A new attack (`attacks/`)

1. Add `attacks/<name>.bt`: `include` the `lib/` behaviours it needs, reference
   them by name (overriding params as needed) and/or inline simple ones, and
   supply `run` + optional `env` + `root`.
2. Preview the resolved TOML: `./bt.py --resolve --include-path lib attacks/<name>.bt`.
3. If it should be deployed, add a `CONFIG_MAP` entry to the `Makefile`, run
   `make configs`, and commit **both** the `.bt` source and the generated TOML.
4. `make check` must pass.

### A new leaf action (Rust)

A new *kind* of effect is a code change in `shared-consensus`, not a `.bt`:

1. Add a variant to `ActionSpec` (`behaviour/registry.rs`) with its params.
2. Add a `LeafAction` impl under `behaviour/actions/<name>.rs` whose
   `contribute()` writes the relevant `ControlSignal` fields; register it in
   `build_action` (`behaviour/tree/actions.rs`).
3. If it needs a new `ControlSignal` field, add it (`behaviour/tree/control.rs`)
   and the mechanical actuator that reads it (consensus state machine or
   `net-core`/`net-node` send path).
4. Tests first (TDD). Then expose it as `Action("<name>", …)` in a behaviour.

See the leaf-action contract for the full recipe.
