---
id: T-0102
title: Web dashboard beta — pair from a browser, see every machine
phase: 4
priority: 4
status: proposed
depends_on: [T-0101, T-0056, T-0085]
scope:
  - crates/arreo-relay/src/web.rs
  - web/**
  - docs/web.md
  - .loop/evidence/T-0102/**
verify:
  - cargo test --workspace
  - browser verification (below)
---

## Goal

ROADMAP §3.10 and §6 Phase 4: "Web dashboard beta (3.10): WASM core, WebTransport (+ WS
fallback), pairing from browser, all-machines overview under our domain (managed relay users)."

One sentence: a browser pairs as a device (WebCrypto keypair, non-extractable where available),
talks WebTransport to the relay with a WebSocket fallback, and shows the fleet — machines,
agents, states, RAM, the blocked questions.

## Acceptance criteria

- [ ] The relay serves WebTransport (HTTP/3) and a WebSocket fallback carrying the **same**
      envelopes; a browser client that can only do one of them sees the same fleet. The fallback
      does not weaken E2E (the payload layer is unchanged) and that is asserted by a scan: the
      relay's state and logs carry no plaintext, as T-0029/T-0051 established for the native
      path.
- [ ] Browser pairing: the same code/QR ritual, a device keypair generated with WebCrypto,
      stored in IndexedDB, the device visible in `arreo devices` and **revocable like any
      other**, auto-expiring by default.
- [ ] The overview renders all machines and agents with states and RAM, and a blocked agent's
      question is answerable from the browser through T-0094's action path.
- [ ] Verification is **in a browser**, not in a test: the harness drives a real Chromium tab
      against a real relay and a real daemon, and the evidence is a capture of the overview plus
      the transcript of a question answered from the page. A unit test on the client logic does
      not close this task.
- [ ] Serving under the managed domain is out of scope (Phase 5's relay work): this runs against
      a self-hosted relay and says so in `docs/web.md`.

## Notes

- Big task, and deliberately last in this batch: it depends on T-0101's wasm subset, on the
  relay's directory (T-0056) and on the release slice (T-0085) for anything shipping.
- If the WebTransport half turns out to need an HTTP/3 server crate the relay does not have,
  that is a ledger decision (new dependency) and the WebSocket path ships first with the other
  marked honestly.
