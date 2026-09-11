# `arreo machines` — the account's machine directory

The machine directory (ROADMAP §3.7) lives at the relay: it holds names, ids and
presence, and nothing else. `arreo machines` is how a human reads it and how a
script consumes it.

```console
arreo machines list   [--json] [--all] [--offline] [--config PATH]
arreo machines status [<name>] [--json] [--offline] [--config PATH]
```

Both verbs read the relay **directly** with this machine's paired device
identity (`identity/device.key` plus the certificate `arreo pair` saved). No
daemon needs to be running, and no socket verb is involved: the CLI speaks the
same `machines` request the daemon does (T-0056).

## The script contract is `--json`, and only `--json`

```json
{
  "schema": 1,
  "source": "relay",
  "as_of": "2026-09-11T09:30:00Z",
  "machines": [
    {
      "name": "workbox",
      "machine_id": "4f55168f55a0019e713e42f853e7be3d",
      "presence": "online",
      "last_seen": "2026-09-11T09:29:58Z",
      "age_secs": 2,
      "proto_version": 0,
      "flags": []
    }
  ]
}
```

- Keys are `snake_case`; rows are sorted by `name`.
- `machine_id` is the **bare** 32 hex characters, the same spelling the
  certificate and the directory use (never the `dev_` display form).
- `presence` is a closed enum: `online`, `offline`, `stale`, `unknown`.
  `unknown` is reserved and is never emitted for a row the relay answered with —
  the relay's rule (T-0043) has three outcomes, and a fourth would be a claim
  nobody made.
- `flags` is the only open-ended field: `name-suffixed` (the relay's collision
  rule gave this machine a suffix — not a mistake), `name-reclaimable` (the
  machine is stale, so its name can be taken), `unverified` (the row came from
  the cache, see below).
- **Schema 1 is additive-only.** A checked-in contract test fails on a removed
  or renamed key and on an out-of-enum presence, so breaking a script is a red
  build rather than a surprise.

**The human table is explicitly not a contract.** Columns, colours and wrapping
may change at any time without a schema bump. Parse `--json`, or the socket API.

## Offline behaviour is honest

`last_seen` is when the machine was last *seen* — a fact about the past. Reading
the directory is a separate act, and when it fails you get the last known rows
plus a statement of what they are:

| Situation | Output | Exit |
| --- | --- | --- |
| Relay answers | the rows, `"source":"relay"` | 0 |
| Relay unreachable | the last known rows, `"source":"cache"`, one warning on stderr | 4 |
| `--offline` | the last known rows, `"source":"cache"` | 0 |
| Relay refuses (bad name, no such account) | the reason | 5 |

**A cached row is never printed as `online`.** Liveness is the one claim the CLI
cannot substantiate with the relay unreachable, so a remembered `online` is
reported as `offline` with the `unverified` flag, and the age tells the rest of
the story. `stale` survives the trip: it is a statement about the past and cannot
become wrong by waiting.

The cache is a mirror, never a source (T-0043's invariant): it is replaced whole
by every successful read, and it lives at `identity/machines.cache` beside the
device key.

## Configuration

`--config PATH`, else `$ARREO_CONFIG`, else
`$XDG_CONFIG_HOME/arreo/arreo.toml` (falling back to `~/.config/…`). The file is
the same one the daemon reads, parsed by the same code
(`arreo_core::relay::config`) — the dependency rule forbids the CLI depending on
`arreo-server`, and a second parser would be a second answer to "what is a valid
`[relay]` section".

```toml
[relay]
enabled = true
addr = "203.0.113.7:443"
account = "acct-1"
```

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | ok |
| 2 | usage (unknown verb, bad argument, incomplete relay config) |
| 3 | no such machine (or a name that is not a name) |
| 4 | the relay is unreachable, or this machine has no paired identity |
| 5 | the relay refused (name conflict, trust refusal) |

## Not implemented yet

`add`, `rename` and `remove` need the directory's **write** side over the wire,
which does not exist yet (tracked as T-0057; T-0056 deliberately landed only the
machine's own row assertion and the account read). They refuse with exit 2 and
name the reason — a verb that looked implemented and silently did nothing would
be worse.
