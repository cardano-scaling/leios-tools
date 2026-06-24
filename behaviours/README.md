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

## How a behaviour tree works (in one minute)

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
| `Action` | `Action("kind", …)` | a leaf that contributes to `ControlSignal` (the catalogue below); `Action("honest")` is the no-op |

## The `.bt` surface language

```
# Comments start with '#'.

include [ "other.bt" ]          # optional: pull in reusable behaviours (resolved by --resolve)

run {                            # required for a *runnable* config (omitted in a pure library fragment)
  name = "my-attack"
  seed = 1234567                 # the one reproducibility seed
}

env {                            # optional: parameters referenced by conditions
  trigger_slot = 345600
}

# Named, reusable behaviours (the name is optional and document-unique):
Selector "strategy" [
  Sequence[
    Condition(cardano.current_slot >= env.trigger_slot),
    ForTicks(3, Action("rb-header-equivocator", ways = 2))
  ],
  Action("honest")
]

root [ "strategy" ]              # required for a runnable config: the entry behaviour
```

- **Children** are either a bare-name **reference** (`"strategy"`) to a
  named behaviour, or an **inline** behaviour. References *expand* to independent
  instances (each gets its own node-local state).
- **Condition grammar:** comparisons (`>= > <= < == !=`), `and` / `or` / `not`,
  and `value.contains(value)`. Values are `env.<dotted.name>`,
  `cardano.<field>` (`current_slot`, `current_epoch`, `mempool_tx_count`),
  integers, or `"strings"`. Every `env.*` / `cardano.*` reference is validated
  (and type-checked) at load time.
- **Pure library fragment** = a `.bt` with **no `run` / no `root`** (named
  behaviours only), meant to be `include`d by an attack that supplies `run`,
  `env`, and `root`. See `lib/long-range-fork.bt`.

## Action catalogue

Each `Action("kind", …)` materialises a leaf that writes its slice of the
`ControlSignal`. `Action("honest")` is the dedicated no-op leaf (it resolves to
the engine's `HonestAction`).

| `kind` | Parameters (defaults) | `ControlSignal` effect |
|---|---|---|
| `honest` | — | none (the honest default) |
| `lazy-voter` | `reason` (`declined`) | `leios.vote = Abstain(reason)` — never casts a CIP-0164 vote |
| `rb-header-equivocator` | `ways` (`2`) | `praos.production = Equivocate{ways}` + `praos.outbound = EquivocateRouting` — N RB variants/slot, routed per peer |
| `deep-reorg` | `every_slots`, `depth` | `praos.reorg_depth = Some(depth)` on due slots — periodic self-reorg + fork |
| `drop-inbound-peers` | `probability` | `praos.drop_inbound` — seeded per-slot reset of inbound peers |
| `t22` | `vote_threshold`, `non_voting_threshold`, `hide_eb_tx_received` | `mempool.tx_filter = ChecksumThreshold{…}` — selectively drop EB-offer processing (t21/t22 selective-withholding) |
| `lie-about-eb-size` | `scale_num` (`1`), `scale_den` (`1`), `offset` (`0`) | `leios.offer_eb_size = Linear{…}` — rewrite advertised `eb_size` to `(size*num/den)+offset` |
| `echo-to-source` | — | `leios.echo_to_source = true` — reflect EB/EB-tx offers back to their source (opens the no-echo gate) |

`reason` accepts the kebab-case `NoVoteReason` values (`declined`, `wrong-eb`,
`late-eb`, …). Defaults come from the action registry (`ActionSpec` in
`shared-rs/consensus/src/behaviour/registry.rs`).

> **sim-rs caveat:** the simulator models EB/RB diffusion as **broadcast**, so
> the per-peer *outbound* adversaries — `rb-header-equivocator` peer-split
> routing, `lie-about-eb-size`, `echo-to-source` — and `drop-inbound-peers`
> have no effect there. `lazy-voter`, `t22`, and `deep-reorg` work in both
> net-rs and sim-rs.

## Existing behaviours

`lib/` — reusable / standalone behaviours (the source of truth for the
generated configs):

| File | Shape | What it does |
|---|---|---|
| `honest.bt` | leaf | no perturbation (≡ no behaviour tree) |
| `lazy-voter.bt` | leaf | always abstains (`reason = declined`) |
| `rb-header-equivocator.bt` | leaf | RB-header equivocation, `ways = 2` |
| `deep-reorg.bt` | leaf | reorg `depth = 5` every `100` slots |
| `drop-inbound-peers.bt` | leaf | reset inbound peers with `probability = 0.15`/slot |
| `t22.bt` | leaf | EB-processing filter, thresholds `80` (voting) / `60` (non-voting) |
| `lie-about-eb-size.bt` | leaf | size-zero EB offer (`0/1/0`) |
| `echo-to-source.bt` | leaf | open the no-echo gate |
| `duplex-follower-bug.bt` | `Join` | the duplex-follower crash: `echo-to-source` + `lie-about-eb-size(0,1,0)` concurrently |
| `long-range-fork.bt` | `Selector` (pure fragment — no `run`/`root`) | honest until `env.trigger_slot`, then equivocate + drop-inbound for 3 slots; `include`-only |

`attacks/` — runnable attacks that compose `lib/` fragments:

| File | What it does |
|---|---|
| `long-range-fork-attack.bt` | `include`s `long-range-fork`, supplies `run`, `env.trigger_slot = 345600`, and `root` |

## The translator (`bt.py`)

Pure-Python, stdlib only (needs Python ≥ 3.11 for `tomllib`). Reads one file
(or `-` for stdin); always writes to **stdout**. Direction is inferred from the
extension unless forced.

```sh
./bt.py file.bt                       # .bt  -> TOML   (direction inferred)
./bt.py file.toml                     # TOML -> .bt    (direction inferred)
./bt.py --bt-to-toml -                # force .bt  -> TOML (stdin -> stdout)
./bt.py --toml-to-bt -                # force TOML -> .bt
./bt.py --resolve --include-path lib attacks/x.bt   # expand includes -> one self-contained TOML
```

`--resolve` is the **build step**: it follows `include`s (by filename, searched
against each `--include-path`), deep-merges into one self-contained config, and
validates that every reference resolves. The engine only loads self-contained
configs — it **rejects** a config that still carries `includes` (run
`bt.py --resolve` first).

## The build process (`make`)

The runnable `lib/*.bt` files are the **single source of truth** for the configs
the node and simulator load. The mapping lives in the `Makefile` (`CONFIG_MAP`):

| Source | Generated config(s) |
|---|---|
| `lib/honest.bt` | `net-cluster/behaviours/honest.toml` |
| `lib/lazy-voter.bt` | `net-cluster/behaviours/lazy-voter.toml` **+** `sim-rs/parameters/behaviours/lazy-voter.toml` |
| `lib/rb-header-equivocator.bt` | `net-cluster/behaviours/rb-equivocator.toml` |
| `lib/t22.bt` | `net-cluster/behaviours/t22.toml` |
| `lib/duplex-follower-bug.bt` | `net-cluster/behaviours/duplex-follower-bug.toml` |

Targets:

| `make …` | Does |
|---|---|
| `configs` | regenerate the consumer configs from their `.bt` sources (each gets a `# GENERATED …` header) |
| `check-configs` | regenerate to a temp and `diff` against the committed configs — fails on drift |
| `test` | `.bt` ⇄ TOML round-trip idempotence over `lib/` + `attacks/` |
| `test-resolve` | compare `--resolve` of the sample attack against the committed golden (`expected/`) |
| `bless-resolve` | regenerate that golden (after an intentional change) |
| `check` | `test` + `test-resolve` + `check-configs` (this is what CI runs) |

CI: [`../.github/workflows/behaviours.yaml`](../.github/workflows/behaviours.yaml)
runs `make check`, so a `.bt` edited without re-running `make configs` fails the
build.

## How consumers select a behaviour tree

- **net-node:** `--behaviour-tree <path-to-resolved.toml>` (or the
  `behaviour_tree` config key). Absent ⇒ an implicit honest tree.
- **net-cluster:** `behaviour_tree = "<path>"` + a `[behaviour_selection]` block
  (`all` / `nodes` / `stake-ordered` / `stake-random` / `stake-fraction`)
  choosing which nodes get it installed at spawn.
- **sim-rs:** under `leios-variant: shared-consensus`, a `consensus-behaviours`
  entry pairs `behaviour-tree: <path>` with a `selection`.

## Authoring guide

### A new behaviour-tree config (`.bt`)

1. Add `lib/<name>.bt` (reusable / standalone) or `attacks/<name>.bt` (runnable,
   composing `lib/` fragments via `include`).
2. Preview the resolved TOML: `./bt.py --resolve --include-path lib <file>`.
3. If it should be deployed, add a `CONFIG_MAP` entry to the `Makefile`, run
   `make configs`, and commit **both** the `.bt` source and the generated TOML.
4. `make check` must pass.

### A new leaf action (Rust)

A new *kind* of adversarial effect is a code change in `shared-consensus`, not a
`.bt`:

1. Add a variant to `ActionSpec` (`behaviour/registry.rs`) with its params.
2. Add a `LeafAction` impl under `behaviour/actions/<name>.rs` whose
   `contribute()` writes the relevant `ControlSignal` fields; register it in
   `build_action` (`behaviour/tree/actions.rs`).
3. If it needs a new `ControlSignal` field, add it (`behaviour/tree/control.rs`)
   and the mechanical actuator that reads it (consensus state machine or
   `net-core`/`net-node` send path).
4. Tests first (TDD). Then expose it as `Action("<name>", …)` in a `.bt`.

See the leaf-action contract for the full recipe.
