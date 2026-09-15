---
id: T-0123
title: Both stores' beta tracks
phase: 3
priority: 4
status: needs-human
depends_on: [T-0119, T-0120, T-0121, T-0122]
scope:
  - docs/mobile.md
  - .loop/evidence/T-0123/**
verify:
  - (store upload tooling — needs accounts and signing identities)
---

## Goal

Phase 3's exit line: "managed from a phone (**both stores' beta tracks**)". TestFlight and
Google Play's internal testing track, with the two builds uploaded and installable by someone
who is not the author.

## Acceptance criteria

- [ ] Both apps uploaded to their beta tracks, with the build number and the review status
      recorded — a screenshot of each track's dashboard in `.loop/evidence/T-0123/`.
- [ ] A **stranger** (not the author) installs both, pairs a machine and reaches the exit demo
      without reading `docs/mobile.md`: the phase's own exit criterion, "30 agents on a server,
      managed from a phone".
- [ ] The privacy declarations are honest about what leaves the device: the relay sees
      ciphertext (T-0029's opacity is the claim), and the app collects nothing else. An
      inaccurate privacy label is a store rejection and a trust problem at once.
- [ ] Signing identities and account access are documented as the human's, not committed — no
      keystore, no key, no password in the repo (PROMPT §5.6).

## Notes

- `needs-human`: store accounts, an Apple Developer membership and the signing identities are
  decisions and purchases, not code. The repo's job is to have everything else ready.
