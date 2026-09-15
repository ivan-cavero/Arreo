---
id: T-0118
title: The pairing payload a camera can scan
phase: 3
priority: 3
status: proposed
depends_on: [T-0024, T-0104]
scope:
  - crates/arreo-core/src/pairing/**
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - .loop/evidence/T-0118/**
verify:
  - cargo test -p arreo-core --lib pairing
  - cargo test -p arreo-core-ffi
---

## Goal

Phase 3's "pairing QR". The invite already exists as a URI (`pairing_invite_uri`, T-0024's
`arreo://pair?…`), and T-0104 exports both the URI and its parser. What has not been done is the
part that makes it *scannable*: a QR has a capacity, a byte mode and an error-correction level,
and an invite that exceeds what a phone camera can read from a screen at arm's length is a
pairing flow that fails in the user's hand.

## Acceptance criteria

- [ ] The invite's encoded size is **measured and bounded**: a test asserts the URI's length for
      the worst realistic case (longest account name, longest machine name, longest relay
      address) and states which QR version and error-correction level that needs. If it does not
      fit a comfortable version, the encoding changes (a compact key ordering, a shorter scheme)
      and the reason is recorded.
- [ ] The round trip is proven at the boundary: `pairing_invite_uri` → the bytes a QR would carry
      → `pairing_invite_parse` gives back the same invite, with the *bytes* (not the string)
      going through, since that is what a camera delivers.
- [ ] The parser's refusals are typed and named (a truncated scan, a wrong scheme, a missing
      field) — a camera reads garbage often, and "invalid invite" with no detail is a bug report
      nobody can act on.
- [ ] `docs/mobile.md` records the size budget and what a UI must do when a scan fails.

## Notes

- No QR *renderer* here: generating the matrix is the UI toolkit's (CoreImage, ZXing), and
  hand-rolling Reed-Solomon to avoid a dependency the platform already ships would be the
  scaffolding-dependency failure in reverse. What this task owns is the payload's fitness.
