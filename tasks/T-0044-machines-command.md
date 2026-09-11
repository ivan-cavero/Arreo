---
id: T-0044
title: "`arreo machines` — list/add/rename/remove/status with a stable script contract"
phase: 2
priority: 2
status: proposed
depends_on: [T-0043, T-0029]
scope:
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/src/machines.rs
  - crates/arreo-cli/tests/machines.rs
  - docs/machines.md
  - .loop/evidence/T-0044/**
---

## Goal

Make the machine directory (§3.7) usable by a human in one glance and by a script without guessing:
`arreo machines list|add|rename|remove|status`, with a versioned `--json` envelope that is a contract
and a human table that deliberately is not. This is the CLI half of "machines are first-class
citizens, not SSH bookmarks".

## Acceptance criteria

- [ ] Verbs exist and are documented in `--help`: `list [--json] [--all] [--offline]`,
      `add <pairing-code> [--name <name>]`, `rename <old> <new>`, `remove <name> [--force]`,
      `status [<name>] [--json] [--watch]`; exit codes stated there and stable: 0 ok, 2 usage,
      3 unknown machine, 4 directory/relay unreachable, 5 name conflict or trust refusal.
- [ ] The script contract is `--json` only: `{"schema":1,"source":"relay|cache","as_of":"<RFC3339>",
      "machines":[{"name","machine_id","presence","last_seen","proto_version","flags":[…]}]}` —
      snake_case keys, `presence` a closed enum (`online|offline|stale|unknown`), sorted by name.
      Schema 1 is additive-only: a checked-in contract test fails on a removed or renamed key and on an
      out-of-enum value, so breaking a script is a red build, not a surprise.
- [ ] The human table is explicitly NOT a contract — columns, colors and wrapping may change at any
      time; said in `--help`, `docs/machines.md` and here. Scripts parse `--json` or the socket API,
      never the table.
- [ ] Offline behavior is honest: with the relay unreachable, `list`/`status` print cached rows with
      `"source":"cache"` and per-row ages, warn once on stderr and exit 4; `--offline` makes that
      intentional (exit 0, `"source":"cache"`); a cached row is never printed as `online`.
- [ ] `add` only completes a join: it admits a machine that displayed an `arreo pair` code (single-use,
      5 min), takes the name from `--name` or the joining side, and prints the granted name (plain or
      the T-0043 suffix) plus the machine fingerprint; it never accepts an arbitrary host/port and
      never silently renames an existing machine.
- [ ] `rename`/`remove` never leave partial state: renaming onto a live name exits 5 with both names
      untouched; removing an unknown name exits 3; `remove --force` performs the T-0043 tombstone
      bypass and prints what it reclaimed; both re-read the directory and print the resulting row.
- [ ] `status <name>` reports the stable subset — presence, last-seen age, negotiated protocol version,
      link path (`relay` | `lan-direct`) and trusted-device count — with that count from T-0046's API
      (`null` until it lands, never a fabricated 0); `--watch` emits one line per change (JSONL with
      `--json`) on a 1 s tick and exits cleanly on SIGINT.
- [ ] Tests: unit coverage for name validation, the JSON contract and exit codes, plus a hermetic
      integration test driving the real CLI against a loopback relay + daemon pair
      (`cargo test -p arreo-cli --test machines`), transcript in `.loop/evidence/T-0044/`.

## Notes

Lives in `arreo-cli`; the existing hand-rolled arg parsing is kept (no clap added by this task) and
every read goes through T-0043's directory API — the CLI never opens SQLite. Rejected: a "stable"
human table (freezes formatting forever for no benefit), YAML output (no consumer), JSON only via env
var (harder to script than a flag), and `machines trust` living here (it ships with the trust
semantics in T-0046 so the two cannot drift). Soft dependencies: the pairing-code session from the
Phase 2 pairing task and relay presence push from the relay task (separate files) — until push lands,
`--watch` polls. Honest gaps: `status` link/RTT is only meaningful on loopback/LAN in v1, and `--all`
is the only way to see tombstoned names, by design.

## Verification

```console
cargo test -p arreo-cli --test machines
cargo fmt --all -- --check
```

## Re-scope (2026-09-11, during T-0043's landing)

**`depends_on` gained T-0029, and the reason is a real prerequisite, not a
preference.** Two criteria in this file cannot be met without the relay actually
serving the directory over a socket:

- `add <pairing-code>` completes an *account join*, and a join needs a
  `JoinTicket` (T-0043) — which today exists only in-process. Nothing issues one
  over the wire yet; that is the relay's router (T-0029).
- The integration test drives "the real CLI against a loopback relay + daemon
  pair", which presupposes the relay has something to serve on loopback.

T-0043 landed the *rules and the durable store*; T-0029 landed the router
(device authentication and envelope routing) — but **neither issues a
`JoinTicket` over the wire.** The missing piece is an account-join RPC: after the
T-0024 pairing flow pins a device, something must ask the relay to admit the
machine, and the relay must mint a single-use ticket for it. That is the
machine-registration half of the daemon's relay client, so it belongs to
**T-0050**, not to a CLI task. Claiming T-0044 without it would mean inventing an
account-join transport inside `arreo-cli` — the wrong file, and the wrong fence.

Confirmed again after T-0029 landed (2026-09-11): the router has no join verb.
T-0044 stays blocked until T-0050 adds one.

`status <name>`'s trusted-device count already declares itself `null` until
T-0046, which is the honest pattern; this note applies the same standard to the
join path rather than shipping a stubbed `add`.
