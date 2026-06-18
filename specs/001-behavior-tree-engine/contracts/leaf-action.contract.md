# Contract: Behaviour / Leaf-Action Interface

The internal Rust contract every behaviour honors, and the recipe engineers follow to add
a new leaf action. Architecture: [`../design/unified-tick-model.md`](../design/unified-tick-model.md).
The old `Behaviour` hook trait is removed; leaves are **control-signal contributors**.

## Tick contract

```rust
/// Tick the tree once (one slot advance). Pure w.r.t. I/O: reads env + read-only chain
/// state, mutates internal Running memory, returns a status and the slot's ControlSignal.
/// No clock reads, no thread_rng. This is the ONLY place decisions are made.
fn tick(&mut self, ctx: &TickCtx) -> (Status, ControlSignal);
```

Guarantees:
- Returns exactly one `Status` (FR-001).
- Produces exactly one `ControlSignal` for the slot, accumulated from the active leaves in
  deterministic order (parent policy order; `BTreeMap`/`BTreeSet` for map-derived order).
- A behaviour that returned `Running` is re-entered next tick under the reactive rules
  (see `../design/bt-grammar-and-semantics.md`).
- The engine performs **no I/O and makes no consensus calls** — it only computes
  `ControlSignal`. The net-node wrapper applies + publishes it; the consensus/I-O actuators
  read it. There is no second decision path.

## Leaf-action contract (control-signal contributor)

```rust
trait LeafAction {
    /// When this leaf's branch is active this tick, write its slice of the slot's
    /// ControlSignal and return a status. Contributes only; never branches on consensus.
    fn contribute(&mut self, ctx: &TickCtx, out: &mut ControlSignal) -> Status;
}
```

**House rule (gating style)**: all flow gating lives in explicit `Condition` behaviours; a
leaf returns `Running` the whole time it is meant to be active and does **not** branch
its status on `env`/`state`. ("Stop when the mempool is full" is a `Condition` guarding
the action, not a leaf that inspects state and returns `Success`.) The honest fallback
leaf returns `Success`.

- `Honest` → contributes nothing; returns `Success` (leaves `ControlSignal` at default).
- A re-homed catalogue leaf writes the matching domain field, e.g.:
  - lazy-voter → `out.leios.vote = Abstain(reason)`; `Running`.
  - rb-header-equivocator → `out.praos.production = Equivocate{ways}`,
    `out.praos.outbound = EquivocateRouting{slot, ways, seed}`; `Running`.
  - deep-reorg → `out.praos.reorg_depth = Some(depth)` on the due slot; `Running`.
  - drop-inbound-peers → `out.praos.drop_inbound = draw(seed, slot) < p`; `Running`.
  - t22 → `out.mempool.tx_filter = ChecksumThreshold{..}`; `Running`.

Leaves are constructed by the retained registry: a BT config names a leaf by `kind`
(+ params); `build(kind, params, seed)` returns the `LeafAction`.

**Ownership**: a *behaviour* owns its config struct (+ `Deserialize`), `contribute()`, and
tests in **one file**. An *actuator* owns its `ControlSignal` sub-struct
(`praos`/`leios`/`mempool`). A behaviour reusing an existing actuator adds no
`ControlSignal` field; only a new effect kind does. (Rationale: research D10.)

## Determinism (non-negotiable — inherited from shared-consensus)

- No `Instant::now()` / `SystemTime::now()` in behaviour logic; drive timing off
  `ctx.state.current_slot`.
- Randomness only via the config `seed` threaded through `blake2b_simd`
  (`child_seed`/`seed_from_node_id`); never `thread_rng`/`from_entropy`.
- `BTreeMap`/`BTreeSet` for ordered state; no `HashMap` iteration in ordered paths.

## Actuator contract (mechanical, no decisions)

The points that consume `ControlSignal` (`leios`/`praos`/`mempool` vote/tx paths,
`production.rs`, `net-core server_handlers.rs` outbound, `main.rs` reorg/drop) MUST:
- read the relevant `ControlSignal` field (or per-slot policy field set by
  `apply_control`) and apply it mechanically;
- contain **no** branching on env/state of their own — all such decisions belong to the
  tick.

## Adding a new leaf action

1. Add a variant/case under `ActionKind` (`tree/actions.rs`) carrying its parameters,
   and register its `kind` in the registry `build`.
2. Implement `contribute`: read `ctx`, write its `ControlSignal` fields, return a status.
3. If it needs a new control-signal field, add it to `ControlSignal` (`tree/control.rs`) and
   add the mechanical actuator that reads it at the right interception point.
4. Tests (TDD, NON-NEGOTIABLE): a failing test first — assert the produced `ControlSignal`
   and status for representative env/state; a "same seed → same ControlSignal" determinism
   test; and at least one malformed-config rejection test.
5. Document the `type`/fields in `bt-config.schema.md`.

## Error handling

- No panics in non-test code (net-rs rule): every `unwrap`/`expect`/index is replaced
  with `Result`/`Option` propagation or justified unreachable with a comment.
- Load/validation errors are `Result::Err` naming the offending id/field; `tick` never
  fails for config reasons (all config errors are caught at load).
