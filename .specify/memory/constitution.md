<!--
SYNC IMPACT REPORT
==================
Version change: 1.0.0 → 1.1.0
Bump rationale: MINOR — materially expanded guidance to Principle I (Test-Driven
Development): added that AI must not change tests to match the code without human
approval. No principle removed or redefined; no governance change. Templates
unaffected (the TDD-mandatory guidance they already carry still holds).

Prior: (unratified template) → 1.0.0 — Initial ratification. The constitution file
previously contained only unfilled template placeholders; this was the first
concrete adoption, so the version started at 1.0.0 per semantic versioning.

Modified principles (placeholder → concrete):
- [PRINCIPLE_1_NAME] → I. Test-Driven Development (NON-NEGOTIABLE)
- [PRINCIPLE_2_NAME] → II. Verified Test Coverage
- [PRINCIPLE_3_NAME] → III. Adversarial Red-Team Focus
- [PRINCIPLE_4_NAME] → IV. Idiomatic Rust
- [PRINCIPLE_5_NAME] → V. Automated Quality Gates

Added sections:
- Additional Constraints (Rust toolchain & workspace layout)
- Development Workflow (commit discipline & review gates)

Removed sections: none

Templates requiring updates:
- ✅ .specify/templates/plan-template.md — Constitution Check is a generic
  placeholder; compatible, no edit required.
- ✅ .specify/templates/spec-template.md — no test-optionality or
  principle-driven references present; compatible, no edit required.
- ✅ .specify/templates/tasks-template.md — updated "Tests are OPTIONAL"
  guidance and per-story test headers to reflect NON-NEGOTIABLE TDD, and added a
  fmt/clippy quality-gate task.
- ✅ .specify/templates/checklist-template.md — no principle-driven references;
  no edit required.

Follow-up TODOs: none. Ratification date set to first adoption (2026-06-15).
-->

# leios-tools Constitution

## Core Principles

### I. Test-Driven Development (NON-NEGOTIABLE)

TDD is mandatory for every change. The Red-Green-Refactor cycle MUST be followed:
write a failing test that captures the intended behavior or defect, confirm it
fails for the expected reason, implement the minimum code to make it pass, then
refactor with tests green. Tests MUST pass before any commit. No production code
is committed without an accompanying test that exercises it, except where Principle
II's manual-confirmation path explicitly applies. AI will not change the tests to match the code without a human approval.

**Rationale**: This is a Red Team effort against consensus-critical software.
Untested behavior is unverified behavior, and unverified behavior cannot be trusted
to either prove or disprove a vulnerability.

### II. Verified Test Coverage

Every change MUST be verified by one of two paths: (a) automated unit and/or
integration tests via `cargo test`, or (b) a manual test whose procedure and result
are explicitly confirmed with the human in the loop. The default and preferred path
is automated tests. The manual path MUST record what was executed and what was
observed; it MUST NOT be claimed as "tested" without that human confirmation.

**Rationale**: Coverage must be real and auditable. Allowing a documented,
human-confirmed manual path keeps progress unblocked for cases that resist
automation while preventing silent, unverifiable claims of correctness.

### III. Adversarial Red-Team Focus

The purpose of this project is to break the Cardano node's Leios extensions. Work
MUST be framed adversarially: tests and tooling SHOULD probe boundary conditions,
malformed and hostile inputs, protocol-rule violations, resource exhaustion, and
timing or ordering anomalies — not just the happy path. Findings (whether a
reproduced defect or a confirmed-safe boundary) MUST be documented so they are
reproducible. Exploit and fuzzing code in this repository is authorized for this
defensive security research only.

**Rationale**: A red-team tool that only tests expected behavior finds nothing. The
value is in systematically exercising what implementers did not anticipate.

### IV. Idiomatic Rust

Code MUST follow industry-standard Rust paradigms. Prefer the type system and
ownership model to enforce invariants; use `Result` and `?` for recoverable errors
and reserve `panic!`/`unwrap`/`expect` for genuinely unreachable states or tests.
Favor iterators, pattern matching, and small composable functions over imperative
sprawl. Public items SHOULD be documented; `unsafe` MUST be justified with a comment
explaining why it is sound. New dependencies MUST be justified and kept minimal.

**Rationale**: Idiomatic Rust is more correct, more reviewable, and less likely to
introduce the very classes of bugs this project hunts for.

### V. Automated Quality Gates

`cargo fmt --check` and `cargo clippy` MUST pass with no warnings before any commit,
alongside the passing test suite from Principle I. These gates are not advisory:
formatting and lint failures block the commit. Gates run per-workspace
(`shared-rs/`, `net-rs/`, `sim-rs/`) since each builds independently.

**Rationale**: Consistent formatting and a clean clippy baseline keep diffs
meaningful and catch correctness and style defects mechanically, before review.

## Additional Constraints

- **Language & toolchain**: Stable Rust via Cargo. Each of the three workspaces
  (`shared-rs/`, `net-rs/`, `sim-rs/`) builds, tests, and lints independently from
  its own directory.
- **Workspace layout**: The relative directory layout MUST be preserved.
  `net-rs` and `sim-rs` depend on `shared-rs` through relative path dependencies,
  and supporting data files are referenced via relative symlinks; restructuring that
  breaks these paths is prohibited without an explicit migration.
- **Testing tools**: `cargo test` is the baseline. Property-based and fuzz testing
  are encouraged where they strengthen the adversarial goals of Principle III.

## Development Workflow

- **Pre-commit gate**: A change is committable only when, for each affected
  workspace, `cargo test`, `cargo fmt --check`, and `cargo clippy` all pass (or the
  human-confirmed manual path of Principle II is satisfied and recorded).
- **Commit discipline**: Commit after each task or logical group, with the working
  tree in a green state. Do not commit code with failing or skipped gates.
- **Review**: Changes SHOULD be reviewed against these principles. Any deviation
  MUST be called out and justified in the change description.

## Governance

This constitution supersedes other development practices for this repository. When a
practice and this document conflict, this document wins.

Amendments MUST be proposed as a documented change to this file, including the
rationale and a version bump. Versioning follows semantic versioning: MAJOR for
backward-incompatible governance or principle removals/redefinitions, MINOR for a
new principle or materially expanded guidance, PATCH for clarifications and
non-semantic refinements. When an amendment changes principles, dependent templates
under `.specify/templates/` MUST be reviewed and synchronized in the same change.

Compliance is verified at commit and review time. Any violation requires either
remediation before merge or an explicit, documented justification. Use `CLAUDE.md`
and the active plan for runtime development guidance.

**Version**: 1.1.0 | **Ratified**: 2026-06-15 | **Last Amended**: 2026-06-17
