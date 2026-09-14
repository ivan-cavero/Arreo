---
id: T-0103
title: Task surfaces — a pane that knows its GitHub issue or Linear ticket
phase: 4
priority: 4
status: proposed
depends_on: [T-0075, T-0091]
scope:
  - crates/arreo-core/src/tasks/**
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - docs/tasks-surfaces.md
  - .loop/evidence/T-0103/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice api
---

## GATE — network credentials required for the live half

The parsing and the model are testable offline against recorded payloads; the live half needs a
token and a reachable API. Probe first, and if there is no token, ship the offline half and say
so:

```console
which gh; gh auth status 2>&1 | head -3
printenv | grep -E '^(GITHUB_TOKEN|GH_TOKEN|LINEAR_API_KEY)' || echo none
```

## Goal

ROADMAP §6 Phase 4: "GitHub/Linear task surfaces". A pane in a worktree (T-0091) is a unit of
work; naming which issue it belongs to is what lets the fleet be read as a board.

One sentence: a pane can carry a task reference (`gh:owner/repo#123`, `linear:ENG-42`), the
sidebar shows its title and state, and completing the work is one key away from updating it.

## Acceptance criteria

- [ ] The reference model and its parsing are offline and tested: a reference is validated
      against a documented grammar, an unparseable one is refused naming the accepted forms, and
      a reference is stored with the pane's record so it survives a restart.
- [ ] The sidebar/listing shows the task's title and state from a **cache**, with the cache's age
      shown (`source: "cache"`, `unverified` — T-0044's honest-cache rule, reused rather than
      re-invented). No network call in a render path.
- [ ] The refresh path is one verb (`arreo task refresh <pane>`), bounded, and its failures are
      the API's own message — never a generic error that hides a 403.
- [ ] Writing back (comment, close, transition) is **explicit and confirmed**, audited with the
      device that did it, and refused when the pane has no reference.
- [ ] Tokens come from the environment or the machine's credential store, never the config file
      and never the sync engine's SYNC class (a token is LOCAL — the T-0083 rule applied, with a
      test that the scan refuses one in a syncable file).
- [ ] Evidence: the offline half with recorded payloads; the live half only if the gate above
      opened, otherwise the probe transcript and an honest `untried` line.

## Notes

- One provider at a time is fine: GitHub first (the repository this product is developed in),
  Linear as the second implementation of the same trait — not a second code path in the core.
