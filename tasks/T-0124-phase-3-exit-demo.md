---
id: T-0124
title: The Phase-3 exit demo — 30 agents from a phone
phase: 3
priority: 3
status: needs-human
depends_on: [T-0121, T-0122, T-0117]
scope:
  - docs/mobile.md
  - .loop/evidence/T-0124/**
verify:
  - (both apps on real devices, against a real 30-agent machine)
---

## Goal

ROADMAP §3.5's exit: "the demo from the tagline — 30 agents on a server, managed from a phone
(both stores' beta tracks)". Everything else in the phase is in service of this one run.

## Acceptance criteria

- [ ] 30 real agents on one machine, and the demo performed from a phone on each platform:
      overview shows all 30 with live states; a blocked agent is answered from the notification;
      a theme pushed from the server changes both the phone and the TUI; a RAM meter tracks a
      pane that is doing work.
- [ ] The run is captured: screen recording or a screenshot sequence under
      `.loop/evidence/T-0124/`, with the machine's own `arreo panes` output beside it so the
      claim and the ground truth are in the same artifact.
- [ ] **The honest failures are recorded too**: what was slow, what needed a second try, what the
      phone could not do. A demo document with no rough edges is a document nobody trusts, and
      Phase 4's priorities should come out of this list.
- [ ] The phase transition is flagged `needs-human` with this evidence, and `.loop/PHASE-DONE.md`
      gains the Phase-3 section.

## Notes

- This is a *gate*, not a task to grind: if it cannot be run, the phase is not done and the
  ledger says so. Per PROMPT §9.5, the phase boundary is a gate the human reviews.
