# Behaviour Tree — Grammar & Semantics

**Status**: Draft for review (2026-06-17). Authoritative definition of how our behaviour
trees evaluate. Encoding-independent (the concrete on-disk form is TOML — see
[`../contracts/bt-config.schema.md`](../contracts/bt-config.schema.md)); this document
describes the *abstract* grammar and the *operational semantics*.

**Audience**: engineers and reviewers who want the evaluation model stated precisely.

> **Model** (rationale + rejected alternatives: research D4, D16): `Sequence` = ordered
> AND, `Selector` = ordered OR, `Join` = concurrent AND (fail-fast). Evaluation is
> **reactive** — every tick re-evaluates a composite from its first child, so a `Condition`
> precondition is re-checked each tick and can `halt` a running subtree.

## 1. Status

Every behaviour, when *ticked*, returns exactly one `Status`:

| Status    | Meaning |
|-----------|---------|
| `Success` | the behaviour achieved its goal this tick (or its condition held). |
| `Failure` | the behaviour could not achieve its goal (or its condition did not hold). |
| `Running` | the behaviour has not finished; it should be ticked again next tick. |

A tick is delivered to the **root** once per slot advance (the slot-driven tick). The
context for a tick is a read-only snapshot of the world — the node's chain state and the
(guarded) env — written `Γ` below.

## 2. Behaviour kinds (informal)

Three families: **composites** (≥1 child), the **`ForTicks` decorator** (exactly one child),
and **leaves** (no children).

| Kind | Family | One-line semantics |
|------|--------|--------------------|
| `Sequence` | composite | ordered **AND** — succeed iff *all* children succeed; fail on the first failure. |
| `Selector` | composite | ordered **OR** / fallback — succeed on the first child that succeeds; fail iff *all* fail. |
| `Join` | composite | concurrent **AND** — tick *all* children each tick; succeed iff all succeed; **fail-fast**: the first child failure halts the rest and fails. Policy is fixed (no `success_policy` field). |
| `ForTicks` | decorator | run `child` for at most `count` ticks; while the child is `Running`, return `Running`; at the budget, `halt` the child and return `Success`. A child `Success`/`Failure` before the budget propagates. Bounds a subtree's active lifetime (e.g. "attack for 2 slots"); `count` is in **ticks** (our tick base is the slot). |
| `Condition` | leaf | evaluate a predicate over `Γ`; return `Success` or `Failure` **immediately** (never `Running`). Used as a precondition inside a `Sequence`. |
| `Action` | leaf | act on the world (contribute to `ControlSignal`); may return `Success`, `Failure`, or `Running`. |

> A `Condition` placed first in a `Sequence` is a **precondition**: its `Failure`
> short-circuits the `Sequence` before the later children run.

## 3. Abstract grammar (EBNF)

```ebnf
document   ::= { behaviour } root            (* named top-level defs, then the entry *)
root       ::= "root" "[" child "]"          (* required; the single entry behaviour *)

behaviour  ::= sequence | selector | join | forticks | condition | action

sequence   ::= "Sequence" [ name ] "[" child { "," child } "]"
selector   ::= "Selector" [ name ] "[" child { "," child } "]"
join       ::= "Join"     [ name ] "[" child { "," child } "]"

forticks   ::= "ForTicks" [ name ] "(" count "," child ")"    (* count >= 1 *)

condition  ::= "Condition" [ name ] "(" predicate ")"
action     ::= "Action"    [ name ] "(" action_id { "," param } ")"

child      ::= reference | behaviour         (* a child is a name OR an inline behaviour *)
reference  ::= name                          (* use of a previously-defined name *)
name       ::= IDENT | STRING                (* T14, "attack" — document-unique *)
```

- Composites (`Sequence`, `Selector`, `Join`) take **one or more** children in declaration
  order; the `ForTicks` decorator takes a positive tick `count` and **exactly one** child;
  leaves (`Condition`, `Action`) take none.
- **Names are optional** and come right after the kind keyword; they are document-unique. A
  child that is a bare `name` is a **reference** to the so-named behaviour (distinct from an
  `action_id`, which appears only inside `Action(...)`).
- **`root` is required** and names the single entry behaviour (reference or inline); multiple
  entries are composed under an explicit `Selector`/`Join`. Behaviours defined but not
  reachable from `root` are allowed but unused (a lint).
- **References expand (template semantics):** each reference resolves to its definition and
  is instantiated as an **independent copy** — the structure stays a tree, and every instance
  has its own node-local state (`ForTicks` `elapsed`, `Join` `succeeded`, action progress) and
  its own `ControlSignal` contribution. A reference to an undefined name is a load-time error.
- `predicate` is the minimal boolean expression language (comparisons, `and`/`or`/`not`,
  membership) over `env.*` and `cardano.*` — see the config schema.
- `action_id` names a registered action (the action registry); `param`s configure it.
- This maps 1:1 to the TOML config: `name` → the `[behaviours.<name>]` key, a `reference` →
  a `children` id, `root` → `[run].root`. The abstract grammar is what those resolve to.

## 4. Evaluation model

### Notation

We write the per-tick evaluation as a **big-step (natural) semantics** judgment:

```text
Γ ⊢ b ⇓ s
```

| Symbol | Reads as | Here |
|--------|----------|------|
| `Γ` (gamma) | the **context** | the read-only snapshot a tick evaluates against — the node's chain state + the (guarded) env. |
| `⊢` (turnstile) | "under … " / "entails" | "under context `Γ`, the judgment holds." |
| `b` | the **term** | the behaviour being ticked. |
| `⇓` (big-step arrow) | "evaluates to" | the whole tick collapses to a final result (vs. small-step `→`, one reduction at a time). |
| `s` | the **result** | the `Status` returned. |

So `Γ ⊢ b ⇓ s` reads *"in context Γ, ticking behaviour `b` evaluates to status `s`."* This
form composes into inference rules (premises above a line, conclusion below). **Caveat:** a
tick is mildly *effectful* — it can mutate a little carried state and emit `ControlSignal`. The
fully faithful judgment would thread those, `Γ ⊢ ⟨b, σ⟩ ⇓ ⟨s, σ′, δ⟩` (state `σ`→`σ′`,
control-signal contribution `δ`); §5 keeps that implicit and gives the rules as pseudocode for
readability.

### Relations

Two relations are defined over behaviours:

- **tick**: `Γ ⊢ b ⇓ s` — ticking behaviour `b` in context `Γ` yields status `s` (and may
  carry a tiny amount of state into the next tick, and — for `Action`s — contribute to the
  slot's `ControlSignal`).
- **halt**: `halt(b)` — abort `b`: recursively stop and reset it. For an `Action` this means
  it ceases contributing its control signal and resets its progress; for a `Condition` it is a
  no-op; for a composite it halts all children (and clears any carried state).

**Reactive evaluation.** A composite re-evaluates from its first child on *every* tick. It
carries **no** "resume cursor" between ticks; the only state that persists between ticks is
(a) a `Join`'s set of already-succeeded children and (b) an `Action`'s own internal
progress. Consequence: a `Condition` precondition is re-checked every tick, and if it flips
to `Failure` the parent `Sequence` halts the (previously running) later children — the
reactive abort. This is what lets "act adversarially **while** the precondition holds" work.

**Determinism.** Children are ticked in declaration order; `Join` aggregates in
declaration order. No clock reads, no `thread_rng` (randomised actions derive from the
run seed). Same `Γ` + same carried state ⇒ same result.

## 5. Operational semantics (per tick)

Pseudocode; `tick` returns a `Status` and may mutate the small carried state noted above.

### Sequence (ordered AND, reactive)

```text
tick(Sequence[c1..cn], Γ):
    for i in 1..n:                      # always from the start (reactive)
        s = tick(ci, Γ)
        if s == Failure:
            for j in i+1..n: halt(cj)   # abort anything after the failure
            return Failure
        if s == Running:
            for j in i+1..n: halt(cj)   # children after the running one are inactive
            return Running
        # s == Success: fall through to the next child
    return Success                      # all children succeeded
```

### Selector (ordered OR / fallback, reactive)

```text
tick(Selector[c1..cn], Γ):
    for i in 1..n:                      # always from the start (reactive)
        s = tick(ci, Γ)
        if s == Success:
            for j in i+1..n: halt(cj)
            return Success
        if s == Running:
            for j in i+1..n: halt(cj)
            return Running
        # s == Failure: try the next child
    return Failure                      # all children failed
```

### Join (concurrent AND, fail-fast)

"Concurrent" in this discrete-tick model means *every still-pending child is ticked once
per parent tick* (not OS threads). A child that has succeeded is held done until the
`Join` resets.

```text
state: succeeded ⊆ children            # persists across ticks until reset/halt

tick(Join[c1..cn], Γ):
    for ci in (c1..cn where ci ∉ succeeded):   # declaration order
        s = tick(ci, Γ)
        if s == Failure:
            for c in c1..cn: halt(c)    # FAIL-FAST: kill all remaining children
            succeeded = ∅
            return Failure
        if s == Success:
            succeeded += ci
    if succeeded == {c1..cn}:
        succeeded = ∅                   # reset so the node can run again if re-entered
        return Success
    return Running                      # some children still Running, none failed
```

### Condition (leaf, immediate)

```text
tick(Condition(p), Γ):
    return Success if eval(p, Γ) else Failure     # never Running
halt(Condition) = no-op
```

### Action (leaf, may run multiple ticks)

```text
tick(Action(a), Γ):
    s = a.step(Γ)                       # advances the action; contributes to ControlSignal
    return s                            # Success | Failure | Running
halt(Action(a)) = a.stop()             # stop contributing its control signal; reset progress
```

An `Action` contributes its slice of the slot's `ControlSignal` on the ticks where it is
reached and returns `Running` (or `Success` while active). A halted `Action` contributes
nothing on the next tick, so the world reverts toward honest — this is how a reactive abort
removes an adversarial effect. (See the control-signal seam in
[`unified-tick-model.md`](./unified-tick-model.md) and `../data-model.md`.)

### ForTicks (decorator, duration cap)

Runs `child` for at most `n` ticks of active life, then stops it. `elapsed` is node-local
carried state (like `Join`'s `succeeded`), reset by `halt` — so a re-selected `ForTicks`
re-arms.

```text
state: elapsed = 0                     # active ticks so far; persists; reset by halt

tick(ForTicks[n, child], Γ):           # n >= 1
    if elapsed >= n:                   # budget already spent: stable "done"
        halt(child)
        return Success
    s = tick(child, Γ)
    elapsed += 1
    if s == Running and elapsed < n:
        return Running                 # within budget, keep going
    if s == Running:                   # hit the budget on this tick
        halt(child)
        return Success
    return s                           # child finished early: propagate Success/Failure
```

Intended for a continuous (`Running`) child — it bounds the subtree's active lifetime to
`n` ticks (e.g. `ForTicks(2, Join[...])` = "run this attack for two slots").

### halt (abort) — summary

```text
halt(Sequence|Selector S) = for c in S.children: halt(c)
halt(Join P)          = (for c in P.children: halt(c)); P.succeeded = ∅
halt(ForTicks[_, child]) = halt(child); elapsed = 0
halt(Condition)           = no-op
halt(Action a)            = a.stop()
```

## 6. Worked trace

Tree (abstract form):

```text
Selector[
  Sequence[ Condition(cardano.current_slot >= env.trigger_slot),
            Action(rb-header-equivocator, ways = 2) ],
  Action(honest)
]
```

Let `T = env.trigger_slot`. The root is ticked once per slot.

| Slot | Selector → Sequence | Condition | equivocator Action | honest Action | Root status | ControlSignal |
|------|---------------------|-----------|--------------------|---------------|-------------|------------|
| `< T` | Seq ticks Condition first | `Failure` (slot < T) | halted (not reached) | ticked → `Success` | `Success` | honest (default) |
| `= T` (first) | Condition passes, Seq ticks Action | `Success` | `Running` (contributes equivocation) | halted by Selector | `Running` | equivocation |
| `> T` | same as above | `Success` | `Running` (re-contributes) | halted | `Running` | equivocation |
| flip: `trigger_slot` raised over current slot (e.g. via REST) | Condition re-checked, now fails | `Failure` | **halted** (`stop()`) → stops contributing | ticked → `Success` | `Success` | honest again |

The last row is the reactive abort: re-checking the precondition each tick halts the
running adversarial Action and the tree falls back to honest — no special "stop" wiring,
just AND-Sequence + reactive evaluation.

## 7. Properties

- **Exactly one status.** Every `tick` returns exactly one of `Success`/`Failure`/`Running`.
- **Termination of a tick.** A tick visits a finite prefix of each composite's children
  (Sequence/Selector stop at the first non-`Success`/non-`Failure` respectively; Join
  visits each pending child once), and leaves return without recursion, so a tick over a
  finite tree terminates.
- **Reactivity.** Preconditions are re-evaluated every tick; a precondition flip halts the
  affected subtree on the next tick (slot-boundary granularity).
- **Determinism.** See §4.
- **Honest by default.** If no Action is reached-and-running, the slot's `ControlSignal` are
  default (honest). Halting an Action restores honesty for that slice.

## 8. Relation to `ControlSignal` and the single-decision-path model

A whole-tree tick yields `(Status, ControlSignal)`: the `Status` of the root plus the
`ControlSignal` accumulated from the Actions reached and left active this tick. This is the
"decide in the tick" half of the design; the consensus *actuators* then consume `ControlSignal`.
Halted Actions drop out of the accumulation, so the control-signal set always reflects exactly
the currently-active leaves. (See [`unified-tick-model.md`](./unified-tick-model.md).)

## 9. Examples — the existing adversaries

The faithful translation of each shipped adversary is a single `Action`: these behaviours
are "always on" when installed, and un-overridden `ControlSignal` fields stay honest by
default, so no explicit fallback is needed (`action_id` is the registry `kind`; `reason` and
`ways` have defaults). The example below is a **complete document** — (optionally named)
top-level definitions, then the required `root` — showing reuse by name (`"T14"` is
referenced from `"attack"`) and a definition (`gated`) that `root` never reaches, so it is
unused. References **expand** to independent instances (§3).

```text
Action "deep-reorg" ("deep-reorg", every_slots = 50, depth = 10)
Action "honest" ("honest")

# gated — honest until a trigger slot, then equivocate. Defined but unreachable from root → unused.
Selector "gated" [
  Sequence[ Condition(cardano.current_slot >= env.trigger_slot),
            Action("rb-header-equivocator", ways = 2) ],
  "honest"
]

# composite — reorg + inbound reset concurrently after a trigger (replaces old Composite)
Sequence "T14" [
  Condition(cardano.current_slot >= env.trigger_slot),
  Join[ "deep-reorg",                                       # reference (expands to its own instance)
        Action("drop-inbound-peers", probability = 0.5) ]
]

Join "attack" [ "T14", Action("rb-header-equivocator", ways = 2) ]

root [ Selector[ "attack", "honest" ] ]
```

`ControlSignal` each drives: lazy-voter → `leios.vote = Abstain`; rb-header-equivocator →
`praos.production = Equivocate` + `praos.outbound = EquivocateRouting`; deep-reorg →
`praos.reorg_depth`; drop-inbound-peers → `praos.drop_inbound`; t22 → `mempool.tx_filter`;
lie-about-eb-size → `leios.offer_eb_size`; echo-to-source → `leios.echo_to_source`.

**Internal gating stays in the action.** deep-reorg (periodic) and drop-inbound-peers
(stochastic) self-gate deterministically from `(seed, slot)` — they are *not* `Condition`s
because the grammar (§3) has no modulo or randomness. The BT does coarse gating via
`Condition`s over `env`/`state`; periodic/stochastic timing lives in the seeded action.

**Composition.** The old `Composite` becomes a `Join` (run all): deep-reorg and
drop-inbound-peers write distinct `ControlSignal` fields, so they compose without conflict.
Use `Sequence`/`Selector` when you want AND/OR ordering instead of concurrency.

The duplex-follower bug — a size-zero EB offer reflected back to its source — composes the
two merged Leios actions:

```text
root [ Join[ "echo-to-source",
             Action("lie-about-eb-size", scale_num = 0, scale_den = 1, offset = 0) ] ]
```

They write distinct `LeiosControl` fields (`echo_to_source` and `offer_eb_size`), so both
take effect with no ordering. This is the case the old hook composition handled awkwardly
(first-non-`Continue`-wins meant the size mutation only ran if the echo gate fired); as
independent fields under a `Join`, the awkwardness disappears.

### Concrete TOML

The "gated" example above as a `[behaviours.<id>]` config (encoding per
[`../contracts/bt-config.schema.md`](../contracts/bt-config.schema.md)):

```toml
[run]
name = "slot-trigger equivocator"
seed = 1234567
root = "root"

[env]
trigger_slot = 345600

[behaviours.root]
type = "Selector"
children = ["attack", "honest"]

[behaviours.attack]
type = "Sequence"
children = ["cond", "equivocate"]

[behaviours.cond]
type = "Condition"
expression = "cardano.current_slot >= env.trigger_slot"

[behaviours.equivocate]
type = "Action"
spec = { kind = "rb-header-equivocator", ways = 2 }

[behaviours.honest]
type = "HonestAction"
```
