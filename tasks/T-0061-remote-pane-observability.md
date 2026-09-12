---
id: T-0061
title: Remote panes are not second-class in the sidebar — machine, link and the question payload
phase: 2
priority: 3
status: in-progress
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

- [x] The TUI's model carries the target's machine name and link path, and the sidebar shows
      them once per target — as its title, not a per-row column: one sidebar is one machine's
      panes, so a repeated column would be ten copies of one fact. The CLI's `--machine`/`--config`
      flags now work on `arreo-tui` too, resolving through the same `arreo_core::mesh::resolve`
      the CLI uses (moved there from `arreo-cli` so both clients share one answer to "what does
      this name mean"). A local session says "this machine · socket" — a hostname would be a
      different fact, and `Target` deliberately has no `machine()` to lie with.
- [x] A pane whose state is `question` shows **the question** — the last non-empty line of its
      hot ring, which *is* an agent's prompt — indented under the pane, cut to the sidebar's
      width with the cut marked (`…`), and whole in the pane view. The read is
      `arreo_tui::client::asking_line`, the same call on either transport, fetching only for a
      pane already known to be asking (an ordinary cycle costs what it cost before). A pane
      asking by *silence* shows the state and no text, rather than an invented prompt.
- [x] A remote question reaches the sidebar **without the operator doing anything**: the relay
      slice's pane prints its prompt, the engine infers `question`, and the next poll cycle puts
      it in the sidebar — asserted on the reconstructed pty screen after a timeout, not by
      driving a refresh.
- [x] Tested at the level the claim needs: the relay slice drives the real `arreo-tui` on a real
      pty against a peer machine over a real relay, reached **by name**, and asserts the machine,
      the link, the question in the sidebar, the marked cut, and the whole question in the pane
      view. Frames in `.loop/evidence/T-0061/`. A relay-slice test proves the read crosses the
      relay (parity with the peer's own socket); the sidebar/page render is proven load-bearing by
      mutation — suppressing the question line fails two slice checks.
- [x] A remote pane and a local pane with the same state render identically apart from the
      session header, because there is one render path and one `PaneView`: the sidebar reads the
      same fields whichever transport filled them (`--remote`, `--machine`, or a local socket),
      and the slice's local frame (`.loop/evidence/T-0061/03-local-session-sidebar.txt`) shows the
      same groups, RAM and question line as the remote one — the title is the only difference.

## Done

Delivered. `arreo-tui` takes `--machine NAME [--config PATH]` (mutually exclusive with
`--remote/--peer/--socket`: one names a machine, the others an address), resolving through
`arreo_core::mesh::resolve` — the resolver moved out of `arreo-cli` so both clients share one
answer and `arreo-tui` does not depend on a binary crate. The sidebar's title is the session
label; a `question` pane shows the line it is waiting on, cut to width with the mark.

Two things the work taught, both recorded because they will recur:

- **A `Target` cannot name a machine.** The name lives in the directory; a remote target carries
  only the device id. An accessor returning that id would have been read as a name by every
  caller, so `Target::link()` exists and `machine()` deliberately does not — the name travels with
  the resolution. The address-addressed form (`--remote`) labels itself "device ab12cd34 · relay",
  which is what is actually known there.
- **Two live sessions for one device id displace each other at the relay** (T-0060). The first
  version of the slice ran the by-name TUI alongside the address-addressed one and flaked: the
  product was right and the test was asking for something impossible. The block now runs after the
  first TUI quits.

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
