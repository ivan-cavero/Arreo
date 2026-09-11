---
id: T-0061
title: Remote panes are not second-class in the sidebar — machine, link and the question payload
phase: 2
priority: 3
status: proposed
depends_on: [T-0032, T-0045]
scope:
  - crates/arreo-tui/src/ui.rs
  - crates/arreo-tui/src/model.rs
  - crates/arreo-tui/src/main.rs
  - crates/arreo-tui/tests/sidebar.rs
  - .loop/evidence/T-0061/**
---

## Goal

ROADMAP §3.7's observability bar: *"remote panes appear with the same fields as local
ones (state, RAM, machine name, link path), and a remote `question` state surfaces in the
local sidebar with its payload — no degraded second-class display for remote panes."*

The state and RAM halves already hold: the sidebar reads them from whatever client it holds,
and T-0032 made that client work over the relay, so a remote pane shows its state and its RAM
today. Two things are missing, and one of them is the flagship moment (§3.11: *"the agent is
asking YOU"*):

1. **Which machine, and over what.** The TUI knows its `Target` (`Target::describe()` answers
   "peer via relay addr"), but the pane rows do not carry it, so a sidebar of ten panes from
   two machines does not say which is which.
2. **What is being asked.** A pane in `question` blinks in the sidebar and nothing else: the
   payload — the prompt the agent is blocked on — is not shown, locally or remotely. For a
   remote pane that is worse, because the operator cannot walk over and look.

## Acceptance criteria

- [ ] The TUI's model carries the target's machine name and link path, and the sidebar shows
      them once per target (a header line, not repeated on every row — the panes in one
      sidebar are one machine's, today, and a repeated column would be ten copies of one fact).
- [ ] A pane whose state is `question` shows **the question**: the payload the daemon already
      reports for that state, truncated to the sidebar's width, with the full text available in
      the pane view. Locally and remotely the same — one code path, whatever carried the session.
- [ ] A remote question reaches the sidebar **without the operator doing anything**: the daemon
      emits the state change and the payload (T-0004's state engine already detects it), the
      client carries it over whichever transport, and the sidebar re-renders.
- [ ] Tested at the level the claim needs: a scripted PTY driving the real TUI against a daemon
      whose pane is made to ask a question, asserting the machine name, the link path and the
      question text are all *visible* — the pattern T-0015/T-0016 established for TUI slices,
      with the frame captured to `.loop/evidence/T-0061/`.
- [ ] A remote pane and a local pane with the same state render identically apart from the
      machine/link header: no field is present for one and absent for the other.

## Notes

- **Why this is not T-0045.** That task's criterion 4 asks for exactly this, and its scope fence
  lists `arreo-cli`, `arreo-core` and `arreo-server` paths — no `crates/arreo-tui/**`. Editing
  the TUI under T-0045's id would be a change outside its own fence, which this repo treats as a
  bug even when it looks like an improvement. Splitting is the honest fix, and T-0045's criterion
  now points here.
- The payload already travels: `Message::StateEvent` carries the matched pattern and the daemon's
  `AgentState::Question` is detected by T-0004's engine. What is missing is the display path
  (model → sidebar), which is why this is a TUI task and not a protocol one.
- Rejected: a per-row machine column. One sidebar shows one machine's panes; a column that is
  constant down the list is noise, and the header states it once.

## Verification

```console
cargo test -p arreo-tui
cargo xtask e2e --slice tui
```
