---
id: T-0096
title: Community adapter SDK — a third-party TOML that cannot be wrong quietly
phase: 4
priority: 3
status: proposed
depends_on: [T-0017, T-0075]
scope:
  - crates/arreo-core/src/state/adapters.rs
  - adapters/**
  - docs/adapters.md
  - xtask/src/adapters.rs
  - .loop/evidence/T-0096/**
verify:
  - cargo xtask adapters --check
---

## Goal

ROADMAP §6 Phase 4: "community adapter SDK". The adapter registry is already data
(`adapters/*.toml`), which is what makes third-party adapters possible at all — but nothing
tells a contributor what a valid adapter is, and nothing catches the ways one can be *wrong
quietly* (a regex that never matches, a state that cannot be reached, a resume argv that cannot
work).

One sentence: a documented schema plus a linter that fails a bad adapter at load time, so a
community contribution is either correct or loudly rejected — never silently dead.

## Acceptance criteria

- [ ] `docs/adapters.md` documents the schema field by field (harness id, program match, event
      map, resume strategy, question/error patterns), with a **complete worked example** of a
      new harness and the exact command to check it.
- [ ] `cargo xtask adapters --lint <path-or-dir>` reports, per adapter: every pattern compiled
      (a regex that does not compile is an error), every pattern **matched against at least one
      fixture** (a pattern no fixture exercises is a warning naming it — the "never matches"
      failure), every state reachable from some fixture, and every resume argv well-formed.
- [ ] A validator test set: one deliberately broken adapter per failure mode (uncompilable
      regex, unreachable state, empty pattern list, resume kind without argv), each asserted to
      be refused with a message that names the field.
- [ ] `arreo adapters list` / `arreo adapters check <path>` expose the same validation to a
      user who just wrote a TOML, without needing xtask (xtask is dev tooling and never ships).
- [ ] The existing 24-check battery stays green: `cargo xtask adapters --check` unchanged in
      behavior for the shipped adapters.

## Notes

- Inputs: `specs/harness-matrix.md` (T-0075) is the field guide for what a real adapter must
  express; T-0081/T-0082 are the recordings that would populate new ones.
- This task is deliberately **not** gated on a live CLI: it is about the schema and the linter,
  which are testable against fixtures on this box.
