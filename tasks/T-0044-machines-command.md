---
id: T-0044
title: "`arreo machines` — list/add/rename/remove/status with a stable script contract"
phase: 2
priority: 2
status: in-progress
depends_on: [T-0043, T-0029, T-0056, T-0057, T-0058]
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

- [x] Verbs exist and are documented in `--help`: `list [--json] [--all] [--offline]`,
      `status [<name>] [--json] [--offline]`; exit codes stated there and stable: 0 ok, 2 usage,
      3 unknown machine, 4 directory/relay unreachable, 5 name conflict or trust refusal.
      **`add`, `rename` and `remove` moved out (see the re-scope below):** they need transports that do
      not exist, and each names its missing task (`--help` and the refusal message: `rename`/`remove`
      → T-0057, `add` → T-0058) rather than pretending. `--watch` is
      likewise not landed: it belongs with the presence push it would watch.
- [x] The script contract is `--json` only: `{"schema":1,"source":"relay|cache","as_of":"<RFC3339>",
      "machines":[{"name","machine_id","presence","last_seen","proto_version","flags":[…]}]}` —
      snake_case keys, `presence` a closed enum (`online|offline|stale|unknown`), sorted by name.
      Schema 1 is additive-only: a checked-in contract test fails on a removed or renamed key and on an
      out-of-enum value, so breaking a script is a red build, not a surprise.
- [x] The human table is explicitly NOT a contract — columns, colors and wrapping may change at any
      time; said in `--help`, `docs/machines.md` and here. Scripts parse `--json` or the socket API,
      never the table.
- [x] Offline behavior is honest: with the relay unreachable, `list`/`status` print cached rows with
      `"source":"cache"` and per-row ages, warn once on stderr and exit 4; `--offline` makes that
      intentional (exit 0, `"source":"cache"`); a cached row is never printed as `online`.
- [ ] **(moved to T-0058)** `add` only completes a join: it admits a machine that displayed an `arreo pair` code (single-use,
      5 min), takes the name from `--name` or the joining side, and prints the granted name (plain or
      the T-0043 suffix) plus the machine fingerprint; it never accepts an arbitrary host/port and
      never silently renames an existing machine.
- [ ] **(moved to T-0057)** `rename`/`remove` never leave partial state: renaming onto a live name exits 5 with both names
      untouched; removing an unknown name exits 3; `remove --force` performs the T-0043 tombstone
      bypass and prints what it reclaimed; both re-read the directory and print the resulting row.
- [x] `status <name>` reports the stable subset — presence, last-seen age, negotiated protocol version,
      link path (`relay` | `lan-direct`) and trusted-device count — with that count from T-0046's API
      (`null` until it lands, never a fabricated 0); `--watch` emits one line per change (JSONL with
      `--json`) on a 1 s tick and exits cleanly on SIGINT.
- [x] Tests: unit coverage for name validation, the JSON contract and exit codes, plus a hermetic
      integration test driving the real CLI against a loopback relay
      (`cargo test -p arreo-cli --test machines`), transcript in `.loop/evidence/T-0044/`. The
      criteria's "relay + daemon pair" is a relay: the CLI dials the relay itself, so no daemon is in
      the path (see the landing notes).

## Notes

Lives in `arreo-cli`; the existing hand-rolled arg parsing is kept (no clap added by this task) and
every read goes through T-0043's directory API — the CLI never opens SQLite. Rejected: a "stable"
human table (freezes formatting forever for no benefit), YAML output (no consumer), JSON only via env
var (harder to script than a flag), and `machines trust` living here (it ships with the trust
semantics in T-0046 so the two cannot drift). Soft dependencies: the pairing-code session from the
Phase 2 pairing task and relay presence push from the relay task (separate files) — until push lands,
`--watch` polls. Honest gaps: `status` link/RTT is only meaningful on loopback/LAN in v1, and `--all`
is the only way to see tombstoned names, by design.

## Re-scope (2026-09-11, after T-0056 landed the read path)

**Read side landed; three verbs moved to their own tasks, because their transports do not exist and
this task's fence is `crates/arreo-cli/**`.**

Landed here (T-0044's read side, complete): `list [--json] [--all] [--offline]`, `status
[<name>] [--json] [--offline]`, the versioned JSON contract with a checked-in contract test, the
honest offline behaviour with the cache, the exit codes, `docs/machines.md`, and a hermetic
integration test through the real binaries.

Moved:

| Criterion | Moved to | Why it cannot live here |
| --- | --- | --- |
| `rename`, `remove` | **T-0057** | The directory is the relay's, and the relay has no *write* kind beyond a machine asserting its own row (T-0056 deliberately landed only that and the read). A rename needs the collision rule to see the whole account in one transaction — a client-side check would race. Writing a relay RPC under a CLI fence is the wrong file. |
| `add <pairing-code>` | **T-0058** | A row is claimed by a signature the machine makes over its own key (T-0056), and a four-word pairing code authenticates a *pairing session*, not a machine: it carries neither the machine's identity nor the account's coordinates. Deciding how the invite carries those is a protocol decision with an ADR, not a CLI sneeze. |
| `status --watch` | with the presence push | Polling is the fallback the notes already name; a watch loop that polls is a feature nobody asked for, and a watch loop worth shipping waits for the relay's push. |

Re-scoping the fence instead was considered and rejected: extending this task's scope to
`crates/arreo-core/src/relay/**` and `crates/arreo-relay/src/router.rs` would have made one task
own three layers, and the two new tasks are each independently verifiable.

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

**Resolved by T-0056 (2026-09-11):** the join RPC now exists, as its own task because it
is a transport, not a CLI verb. T-0050 landed the daemon's session machinery without the
machine-registration half, so the missing piece was filed and built as T-0056 —
`join`/`machines`/`directory` kinds, the session's request/response path, the relay's
directory handlers, and the daemon asserting its own row on connect and on the presence
cadence. T-0044 is therefore **startable**: `add` completes a join through the daemon's own
relay session and the CLI reads the directory through it (`arreo machines list` needs a
socket verb that proxies to the daemon's `machines` request). T-0056 did deliberately *not*
add a CLI verb: the fence here owns `crates/arreo-cli/src/machines.rs`, and a verb written
there would have been this task's work done under another task's id.

`status <name>`'s trusted-device count already declares itself `null` until
T-0046, which is the honest pattern; this note applies the same standard to the
join path rather than shipping a stubbed `add`.

## Landing notes (2026-09-11)

**The CLI dials the relay itself.** The criteria say "every read goes through T-0043's directory API —
the CLI never opens SQLite", and the CLI holds the same device identity the daemon does
(`identity/device.key` plus the certificate `arreo pair` saved). So `machines` needs no socket verb
and no daemon: one `RelaySession::dial` and one `machines` request. The integration test therefore
drives the real CLI against a loopback relay with no daemon in the path — which is what makes it
hermetic and fast, and it is a stronger test than a proxied one (fewer moving parts, same wire).

**The `[relay]` configuration moved to `arreo-core`.** The CLI needs the relay address and account,
and the dependency rule forbids `arreo-cli` depending on `arreo-server`; the alternative was a second
TOML parser in the CLI. It is now `arreo_core::relay::config` with its own tests, and the daemon
re-exports it, so there is one answer to "what is a valid `[relay]` section".

**A cached row is never `online`.** A remembered row is something the relay told us earlier; liveness
is the one claim the CLI cannot substantiate with the relay unreachable. A cached `online` is reported
as `offline` with an `unverified` flag, and `age_secs` carries when it was really seen — the criterion
says "never printed as online", and the honest way to satisfy it is to refuse the unverifiable claim
rather than to hope the cache is old.

**Found while building it:** the JSON envelope and the human table are two renderings of *one* row
type, deliberately — the first version had the table reading a different struct, which is how a table
starts showing something the contract does not have. And `rfc3339_ms` is hand-rolled in core (twenty
lines of Hinnant's days-to-civil) rather than adding `chrono`/`time` for one JSON field; it is tested
against known instants, a leap day, a non-leap century and a negative epoch.

## Verification
