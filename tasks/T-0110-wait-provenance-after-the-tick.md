---
id: T-0110
title: The ungated tick pump changes what `wait` reports — with notifications off
phase: 4
priority: 2
status: proposed
depends_on: [T-0093]
scope:
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-core/src/state/**
  - crates/arreo-server/tests/notify.rs
  - crates/arreo-server/tests/api.rs
  - .loop/evidence/T-0110/**
verify:
  - cargo test -p arreo-server --test api
  - cargo test -p arreo-server --test notify
---

## Why this exists

Found by the `reviewer` pass over T-0093 and **reproduced** (`agent://NotifyReview`, finding 3).

T-0093's tick pumps every pane once a second so that a pane nobody is attached to is
classified — that is the feature's whole point. The pump is deliberately **not** gated on the
`[notify]` section, and the review shows it is therefore not observationally neutral:

```
$ arreo spawn off-p1 /bin/sh -c 'printf "Proceed? [y/n]\n"; sleep 60'
# 4 s later, with NO [notify] section in the config at all:
$ arreo wait off-p1 --state question
state=Question confidence=direct:already pattern=None
```

Before the tick, the waiting client's own pump produced the `question` event and the reply
carried its real provenance (`inferred:silence+prompt-shape`, `pattern=Some("[y/n]")`). Now the
tick has already classified the pane, the event is gone, and `Wait` takes its
"already in the wanted state" branch — which returns `direct:already` and **drops the matched
pattern**.

Why that is a defect rather than something to document:

- it fires with `[notify]` absent, i.e. for every existing user who never opts in — a behaviour
  change nobody asked for;
- the pattern is, for a `question`, the only field that says *what the pane is asking*, and it
  is what the CLI and the TUI print;
- it removes the honest `inferred:` labelling the state engine exists to provide, and
  "never lie about how a state was derived" is a product rule, not a nicety.

The state **value** is correct and the earlier classification is the improvement the tick is
for; it is the provenance that is lost.

## Acceptance criteria

- [ ] `Wait` answers with the provenance of the transition that produced the state, whether or
      not this process was the one that observed it. The engine already knows how it arrived at
      its state — the fix is to keep that fact (the last transition's `Confidence` and
      `matched_pattern`) alongside the state, and to read it in the "already in the wanted
      state" branch instead of synthesising `direct:already`.
- [ ] `direct:already` survives only where it is *true*: a pane that was already in the state
      before the watch began, with no transition behind it in this engine's life. The
      distinction the field exists for ("the state is the one you asked for, and here is how it
      was derived") must not be collapsed.
- [ ] A regression test that fails on today's code: spawn a pane that asks a question, let the
      daemon's tick classify it with **no `[notify]` section**, then `wait --state question` and
      assert the reply carries the pattern. It must be driven through the real socket, since the
      defect is in the reply's construction.
- [ ] No new wire field: the `Wait` reply's shape stays as it is (T-0028's N−1 window), and the
      fix is what fills `confidence`/`matched_pattern`.

## Notes

- The reviewer's exact reproduction, the `PanesDetail`-races-the-tick analysis, and the
  reasoning for calling it a defect rather than a documentation item are in
  `agent://NotifyReview`.
- Sketched fix shape (for the worker, not a mandate): `Engine` keeps the last transition's
  confidence/pattern (`Option<(Confidence, Option<String>)>`), set wherever it assigns
  `self.state`, and `Wait` reads it when the current state already matches.
