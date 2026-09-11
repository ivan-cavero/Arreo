# The machine's audit trail (schema v5)

> Operator's reference for the append-only log an Arreo daemon keeps beside its
> socket: what it records, what it refuses to record, what it redacts before
> anything reaches disk, and how to read it back. Everything here is a property
> of the code that ships in `crates/arreo-core/src/store.rs`,
> `crates/arreo-server/src/audit.rs`, `crates/arreo-server/src/daemon.rs` and
> `crates/arreo-cli/src/main.rs` — nothing is aspirational, and the sections at
> the end name the parts that are not there yet.

One sentence: the daemon writes one row per action taken against it — who acted,
from which network, what they touched, and whether it was carried out — and the
operator reads those rows back with `arreo audit`.

## 1. What the trail is for

The audit log answers four questions after the fact: **who connected**, **from
where**, **what they did**, and **what was refused**. It is the machine's own
record, written by the daemon that owns the panes, at the moment the action
happens, with the identity the session authenticated as. It is read by a human
(the CLI's table) and by scripts (the `--json` envelope and the export), and it
is written whether or not anyone is watching, so a session that ended badly
still leaves its beginning, its actions and its end on record.

What it is **not**:

- **It is not tamper-evident.** It is append-only *by API* — the store has no
  update and no delete — but there is no hash chain and no signature. Anyone who
  can write to the SQLite file can edit or delete rows and nothing will notice.
- **It is not the relay's log.** This document covers the machine's trail, in
  `<socket>.db`. The relay keeps its own database and its own record; see §8.
- **It is not a capture of everything.** Reads, listings and the size guard's
  own warning leave no row (§3), and the log never holds key material (§4).

## 2. The schema

The log lives in the daemon's sidecar SQLite file, `<socket>.db` — the same WAL
file that holds pane topology, scrollback and the device registry. The default
socket is `$XDG_RUNTIME_DIR/arreo.sock` when that variable is set, otherwise
`$TMPDIR/arreo-<uid>.sock`, so the default log is that path with `.db` appended.
`meta.schema_version` is **5** for this build.

The `audit` table:

| Column | Type | Meaning |
| --- | --- | --- |
| `ts_ms` | INTEGER NOT NULL | Unix milliseconds at the moment of the write |
| `device` | TEXT NOT NULL | The device the row is **about** — always the subject, never the actor, so "everything that happened to this device" is one column query. A device id in display form (`dev_<hex>`), `local-cli` for this machine's own operator on the Unix socket, `daemon` for the enforcement sweeper, or a pairing session id for `pairing.failed` (there is no device yet). Who *performed* an action whose actor is not the subject is in `detail` — see §3 |
| `agent` | TEXT NOT NULL | What the action acted on: a pane id, an agent name. Empty when the action has no object |
| `prompt` | TEXT NOT NULL | The human-readable content — the bytes sent, the program spawned, a refusal reason. Redacted before it is written (§4). Empty when the action has no content |
| `redacted` | INTEGER NOT NULL | `1` when the secret scan found something and the row is flagged, `0` otherwise (§4.1) |
| `kind` | TEXT NOT NULL DEFAULT `'prompt'` | The coarse classification the older readers filter on: `prompt`, `auth_reject`, `device_change`, `pairing_failed`, `unknown` |
| `action` | TEXT NOT NULL DEFAULT `'prompt'` | The event's own name, from the vocabulary in §3 — what an operator greps for |
| `outcome` | TEXT NOT NULL DEFAULT `'ok'` | `ok`, `refused` or `expired` — what tells an intention from a result |
| `peer` | TEXT (nullable) | The peer's network, truncated **at write** (§4.2); NULL when there is none |
| `detail` | TEXT (nullable) | Anything that is not the prompt: a reason, a count, a protocol version |

The table has no declared primary key, so SQLite's implicit `rowid` is what
breaks ties in the ordering (§5).

The version 5 columns arrived in place: v3 added `kind`, v4 added `action`, v5
added `outcome`, `peer` and `detail`. Rows that predate a column keep the
migration's honest default — every existing row was an action that happened
(`ok`) and a prompt (`prompt`), which is exactly what those defaults say.

**The append-only rule.** `SessionStore::record` is the only writer, and the
store exposes no update and no delete for this table. Re-writing the "same"
event appends a new row; it never replaces one. The single removal path is the
operator's explicit, bounded prune, which is itself recorded (§7).

Two parsing rules make the log readable by an older binary: an `outcome` value
that is not `refused` or `expired` reads as `ok`, and a `kind` value that is not
one of the five known names reads as `unknown`. Neither is an error.

## 3. The actions

An action name is a constant, not a free string — the writers, the CLI's filters
and this table all name the same events, so a typo cannot produce a row that no
query finds.

| Action | Written when | `agent` | `prompt` | Outcomes |
| --- | --- | --- | --- | --- |
| `session.connect` | A **remote** session is accepted, before the handshake runs | empty | empty | `ok` |
| `session.disconnect` | A **remote** session's read loop ends; the reason is in `detail` | empty | empty | `ok` |
| `attach` | A client sends `Attach` — or `Resume`, which is recorded under the same name | pane id | empty | `ok` |
| `send` | A client sends `Send` | pane id | the bytes sent | `ok` |
| `spawn` | A client sends `Spawn` | pane id | the program it ran | `ok` |
| `split` | A client sends `Split` | the pane being split | the new pane's id | `ok` |
| `device.issue` | A device is pinned (a certificate is issued for its key) | empty | `issued <role> for <name>` | `ok` |
| `device.rotate` | A device is moved onto a new key | empty | `rotated to <new device id>` | `ok` |
| `device.revoke` | A device is revoked — the first time only, not on a repeat call | empty | empty; `detail` is `revoked by <actor>` | `ok` |
| `auth.reject` | A presented key is refused (no certificate, revoked, or not pinned) | empty | the reason, e.g. `no certificate for this key` | `refused` |
| `pairing.failed` | A pairing attempt produced no certificate | empty | the reason (wrong code, expired window, peer never completed) | `refused` |
| `enforce.breach` | The cgroup guard trips a pane's memory or pid budget | pane id | `<memory\|pids> budget breached` | `ok` |
| `audit.prune` | An operator runs `arreo audit prune` | empty | `removed N row(s) older than MS` | `ok` |
| `prompt` | Nothing writes it today: it is the original T-0018 row and the migration default for rows that predate actions | — | — | `ok` |
| `unknown` | The vocabulary's fallback for a row whose action an older schema did not record; nothing writes it today | — | — | — |

Reading the `device` column takes care, because it is not always the actor:

- For `session.connect`, `session.disconnect`, `attach`, `send`, `spawn` and
  `split`, `device` is the acting identity — the device that authenticated, or
  `local-cli`.
- For `device.issue`, `device.rotate` and `device.revoke`, `device` is the
  **subject**: the device that was issued, rotated or revoked. For a revocation
  the actor is recorded in `detail` as `revoked by <actor>`; for issue and
  rotate the actor is whoever ran the command on the server host and is not in
  the row.
- For `auth.reject`, `device` is the device that was refused.
- For `pairing.failed`, `device` holds a pairing **session** id, because there is
  no device yet — that is why the pairing failed.

**Outcomes.** The vocabulary is `ok | refused | expired`. This build's writers
use `ok` (the action was carried out) and `refused` (it was not, with the reason
in `prompt` or `detail`). `expired` exists for something the machine was holding
running out of time — T-0030's inbox drops — and nothing in this build writes
it; it round-trips if an older or newer binary stored one.

**What is not audited.** The recorded verbs are exactly `attach`, `resume`,
`send`, `spawn` and `split`, plus the rows the daemon and the device authority
write outside verb dispatch. **Reads and listings are deliberately absent** —
`panes`, `snapshot`, `delta`, `read`, `wait`, `metrics` and `metrics-req` leave
no row, because a trail that records every poll is a trail nobody reads, and the
interesting question ("who *did* something") is answered by the writes. Note
that `kill` and `resize` are also absent: they are neither audited nor listed
among the deliberate omissions.

**The local socket asymmetry.** `session.connect` and `session.disconnect` are
written **only for remote sessions**. The local Unix socket writes no such rows,
because the CLI opens one connection per command and one connect/disconnect pair
per invocation would be pure noise. Its *actions* are recorded all the same,
attributed to `local-cli`:

```console
$ arreo spawn pane-a /bin/sh -c 'echo hi; sleep 20'
spawned pane-a
$ arreo send pane-a hello there
$ arreo audit
1789125603963 spawn                  ok       local-cli        pane-a       -                  -                        /bin/sh
1789125605519 send                   ok       local-cli        pane-a       -                  -                        hello there
```

(The `panes` and `read` commands run between those two produced no rows.)

## 4. Redaction

**Redaction happens at write, not at export.** The secret scan runs on the way
in, in `SessionStore::record`, and the peer address is truncated on the way in
too. The guarantee is one decision, not two: a row the scan flags is a row whose
matched secret is masked. Nothing a reader does — no flag, no format, no direct query — can
un-redact what was never stored. A flag someone forgets is a leak; a value never
stored cannot leak.

### 4.1 The secret scan

`scan_secrets` flags lines, and `redact` masks them. The scan's patterns are the
contract:

| Pattern | Flagged as |
| --- | --- |
| `sk-` + 8 more characters | api key (`sk-` prefix) |
| `AKIA` + 8 more characters | aws access key id |
| `ghp_` + 8 more characters | github token |
| `gho_` + 8 more characters | github oauth token |
| `xox` + 8 more characters | slack token |
| `BEGIN PRIVATE KEY`, `BEGIN RSA PRIVATE KEY`, `BEGIN OPENSSH PRIVATE KEY` | private key block |
| `api_key`, `apikey`, `api-key`, `aws_secret`, `client_secret` | secret assignment, when more than 8 characters follow the name |
| `password`, `passwd` | password assignment, when the value after `=`/`:` is 12 characters or more |

A *token* is a prefix followed by at least eight more characters, ending at the
first whitespace or punctuation that cannot be part of one. The scanner and the
masker share that definition; when they disagreed, a line was flagged and stored
unmasked.

The masking rules, applied per line:

- A token is masked wherever it appears in the line, not only when it begins a
  word: `export GITHUB_TOKEN=ghp_…`, `curl -H 'Authorization: token ghp_…'` and
  a bare `sk-…` all become `[REDACTED:token]`. The characters around it stay, so
  the row still reads as the command it was. A prefix followed by a shorter run
  is not a token — `ask-me` contains `sk-` and is left alone, unflagged.
- On a `KEY=VALUE` or `KEY:VALUE` line, the value is replaced when the key part
  contains `key` *and* the value is at least 8 characters — or when the key part
  contains `secret`, `password` or `passwd`, whatever the value's length. The
  key survives and the value becomes `[REDACTED:value]`; everything after the
  first `=`/`:` goes with it.
- A line containing both `BEGIN` and `PRIVATE KEY` becomes
  `[REDACTED:private-key-block]`.
- If the scan flagged something and no rule claimed it, the line is replaced
  with `[REDACTED:secret]`. The fallback is deliberately the safe direction: a
  row that said `redacted=1` while holding the bytes would make the flag a
  statement about the scanner rather than about the log. In this build the
  scanner and the masker agree on what a token is, so the fallback is a
  backstop rather than a routine path.

Field names survive on purpose, so a redacted row is still debuggable:

```json
{"action":"send","agent":"pane-a","prompt":"export OPENAI_API_KEY=[REDACTED:value]","redacted":true}
```

### 4.2 Peer truncation

A peer address is truncated before it is written: IPv4 to a `/24`, IPv6 to a
`/48`. The port is dropped, and so is everything below the network prefix.

| Peer connects from | Stored `peer` |
| --- | --- |
| `203.0.113.77:41000` | `203.0.113.0/24` |
| `10.20.30.40:1234` | `10.20.30.0/24` |
| `127.0.0.1:1` | `127.0.0.0/24` |
| `[2001:db8:1234:5678::1]:41000` | `2001:db8:1234::/48` |
| `[::1]:80` | `0:0:0::/48` |

The reason is the threat model in ROADMAP §4, which includes a compromised cloud
and a stolen phone: a full-address trail is a record of where someone was, and a
location history is not what an audit log is for. A `/24` or `/48` still answers
the question the log exists to answer — "did this come from a network I
recognize". There are no exceptions: loopback and a `/32`-shaped address
truncate too, because an exception is a leak with a justification.

`redact_peer_text` applies the same rule to a peer that arrives as text. It
leaves a string that does not parse as an address unchanged — every writer in
this build hands it a real `SocketAddr`, but the function itself does not
enforce that.

## 5. Reading the log

```console
$ arreo audit [--limit N] [--json] [--socket PATH]
```

- `--limit N` defaults to **50** and means *the newest N rows* (the tail), not
  the first N of history. A non-numeric value is a usage error, exit 2.
- `--json` prints the same tail as one JSON object.
- `--socket PATH` names the daemon socket; the log is read from `PATH.db`. The
  CLI reads that file directly — no daemon round-trip — so the log outlives the
  daemon and can be read while it is stopped. A read never creates the file.
- With no log yet, the tail prints `audit: no log yet (no prompts sent through
  this daemon)` on stderr, prints nothing on stdout, and exits 0. A missing log
  is an empty log, not a broken one.

The table is printed oldest first, newest last, one line per row, with the
prompt's **first line only**:

```console
$ arreo audit
1757000000123 session.connect        ok       dev_4b1e8c2a91f0d3aa              203.0.113.0/24     -
1757000000456 spawn                  ok       dev_4b1e8c2a91f0d3aa pane-a       203.0.113.0/24     -                        /bin/sh
1757000000789 send                   ok       dev_4b1e8c2a91f0d3aa pane-a       203.0.113.0/24     -                        [redacted] export OPENAI_API_KEY=[REDACTED:value]
1757000000900 device.revoke          ok       dev_4b1e8c2a91f0d3aa               -                  revoked by local-cli      -
1757000001000 auth.reject            refused  dev_9f31c0ffee001122              -                  -                        no certificate for this key
1757000001100 session.disconnect     ok       dev_4b1e8c2a91f0d3aa              203.0.113.0/24     connection reset by peer
```

The columns are `ts_ms`, `action`, `outcome`, `device`, `agent`, `peer`,
`detail`, then the prompt. A NULL `peer` or `detail` prints as `-`; a row the
scan flagged carries a `[redacted]` marker before its prompt.

Read as a session, that example says: the device `dev_4b1e8c2a91f0d3aa`
connected from `203.0.113.0/24`, spawned `pane-a`, sent a line into it (redacted
on the way in), and disconnected with `connection reset by peer` as the reason;
the device was revoked in between (the row names `local-cli` as the actor in
its detail); and a second device was refused with
`no certificate for this key`. That is the whole "who, from where, what, and
what was refused" question answered from one command.

`--json` prints one object, oldest first, with the same tail:

```console
$ arreo audit --json --limit 2
{"count":2,"rows":[{"action":"spawn","agent":"pane-a","detail":null,"device":"local-cli","kind":"unknown","outcome":"ok","peer":null,"prompt":"/bin/sh","redacted":false,"ts_ms":1789125603963},{"action":"send","agent":"pane-a","detail":null,"device":"local-cli","kind":"prompt","outcome":"ok","peer":null,"prompt":"hello there","redacted":false,"ts_ms":1789125605519}]}
```

The envelope is `{"count": N, "rows": [ … ]}`. Each row carries exactly ten
fields — `ts_ms`, `action`, `kind`, `outcome`, `device`, `agent`, `prompt`,
`redacted`, `peer`, `detail` — and it is the *same object* the export emits
(§6), so a row read here and a row read from an export cannot disagree. The keys
come out in sorted order; parse by name rather than by position.

With no log yet, `--json` still answers in the shape it promises:

```console
$ arreo audit --json
{"count":0,"rows":[]}
```

Ordering is by `(ts_ms, rowid)`, ascending. The rowid is what breaks a tie, so
several rows written in the same millisecond keep the order they were written —
which is why the trail reads in write order even when a session produces several
actions at once. The timestamp is the primary key of the sort, though: a row
written *after* another but stamped earlier by a clock that stepped backwards
sorts earlier.

## 6. Export

```console
$ arreo audit export [--format jsonl|json] [--since MS] [--until MS] [--action NAME] [--out PATH|-] [--socket PATH]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--format` | `jsonl` | `jsonl` is one JSON object per line (what a log pipeline eats); `json` is a pretty-printed array (what a human reads) |
| `--since MS` | none | Only rows at or after this Unix **millisecond** |
| `--until MS` | none | Only rows at or before this Unix **millisecond** |
| `--action NAME` | none | Only rows with this exact action name (`send`, `device.revoke`, `audit.prune`, …) |
| `--out PATH` | `-` (stdout) | Write to a file instead. A file write prints `exported PATH (format)`; `-` means stdout |
| `--socket PATH` | the default socket | Read the log from `PATH.db` |

Both bounds are **inclusive on both ends**. A non-numeric `MS`, an unknown
`--format`, or an unknown argument is a usage error, exit 2 — a typo'd filter
that silently meant "no filter" is how an export quietly stops being the window
someone asked for.

**The export is a view, never a second source of truth.** It runs the same
filters over the same rows the readers see, so the same window twice is
byte-identical, and the two formats describe the same rows in the same order
(oldest first). `--json` and the export use the same per-row shape, so the CLI
cannot disagree with itself. There is no row limit: an export means the whole
window.

```console
$ arreo audit export --format json --since 1757000000000 --until 1757000000999 --action send
[
  {
    "action": "send",
    "agent": "pane-a",
    "detail": null,
    "device": "dev_4b1e8c2a91f0d3aa",
    "kind": "prompt",
    "outcome": "ok",
    "peer": "203.0.113.0/24",
    "prompt": "export OPENAI_API_KEY=[REDACTED:value]",
    "redacted": true,
    "ts_ms": 1757000000789
  }
]
```

Writing to a file names the path it wrote:

```console
$ arreo audit export --format jsonl --action auth.reject --out /srv/arreo/rejects.jsonl
exported /srv/arreo/rejects.jsonl (jsonl)
```

A machine that has never logged anything still exports parseable bytes: `jsonl`
is empty, `json` is `[]`, and both exit 0. (The stderr note about there being no
log yet is printed in that case, as for every subcommand.)

## 7. Retention and pruning

**Nothing prunes the log automatically.** There is no timer, no size cap and no
rotation: an append-only log that quietly deletes itself is not an audit log,
and the point of the trail is that it outlives the session that wrote it.

What does happen automatically is a **warning**, once, at daemon boot:

```text
arreo-server: the audit log is 120.0 MB across 2 rows (/tmp/arreo-biglog2/arreo.sock.db); nothing prunes it automatically — `arreo audit prune --before <ms>` when you mean to
```

It fires when the stored text reaches **100 MiB** (100 × 1024 × 1024 bytes,
`AUDIT_WARN_BYTES`) and it never prunes anything. The check is a table scan, so
it runs at boot rather than on every write, and it is not repeated while the
daemon is up. The size it reports is the row count and the summed length of the
text columns (`device`, `agent`, `prompt`, `action`, `peer`, `detail`) — not the
size of the SQLite file, which also holds the WAL, the indexes and the other
tables.

Pruning is the operator's explicit action:

```console
$ arreo audit prune --before 1757000000900
pruned 3 row(s) older than 1757000000900
```

- `--before MS` is **required** (Unix milliseconds). Without it the command is a
  usage error, exit 2, with the message
  `audit prune: --before MS is required (nothing prunes the log automatically)`.
- It deletes every row with `ts_ms < MS`, permanently. There is no archive step
  and no undo.
- It then writes its own `audit.prune` row — device `local-cli`, kind `unknown`,
  outcome `ok`, prompt `removed N row(s) older than MS`, detail
  `before_ms=MS removed=N` — so a deletion the log does not mention cannot
  happen.
- **That row is excluded from the prune**, so it survives the prune that wrote
  it, and survives every later prune too. `--before` at the top of the range
  therefore leaves a log that is nothing but prune rows:

```console
$ arreo audit prune --before 9999999999999
pruned 3 row(s) older than 9999999999999
$ arreo audit
1789125517618 audit.prune            ok       local-cli                     -                  before_ms=1757000000900 removed=3 removed 3 row(s) older than 1757000000900
1789125517625 audit.prune            ok       local-cli                     -                  before_ms=9999999999999 removed=3 removed 3 row(s) older than 9999999999999
```

- **A prune that removes nothing still writes its row.** A no-op prune is still
  a write; the operator's intent is part of the history.
- The bound is clamped, not wrapped: `--before` at the top of the `u64` range
  really does mean "everything prunable", rather than silently matching nothing.
- With no log yet, the command still prints its line —
  `pruned 0 row(s) older than MS` — and exits 0, because "the log does not
  exist" and "the log had nothing to drop" are the same answer.

## 8. The machine's log and the relay's

This document covers **the machine's** trail: the rows the daemon writes in
`<socket>.db` for the panes it owns. A session that arrives through a relay is
still recorded here, on this machine, against the device id that authenticated —
but its `peer` is NULL, because a relay peer has no direct address of its own
and what the daemon sees is the relay.

The **relay's own** record is a different database, in the relay's state
directory (`relay.db`), and a different task (T-0053). It is not queryable with
`arreo audit`, and this document does not describe it; `docs/relay-deploy.md`
covers what the relay writes, which today is its own stderr.

## 9. Troubleshooting

| Symptom | Cause | What to check |
| --- | --- | --- |
| `audit: no log yet (no prompts sent through this daemon)` | `<socket>.db` does not exist — nothing has ever been written through that socket | Is `--socket` (or `XDG_RUNTIME_DIR`) pointing at the daemon you mean? A read never creates the file |
| The log is empty but the daemon is clearly working | The work was reads and listings, which are not audited | §3 — `panes`, `read`, `wait`, `metrics`, `kill` and `resize` leave no row |
| A device's `send`/`spawn` rows name `local-cli` | The client connected over the Unix socket, not the network | Attribution follows the transport the session arrived on (§3) |
| `peer` is `-` (NULL) in JSON | A local session, or a session that arrived through the relay | §4.2, §8 |
| A refusal is missing after a device was revoked | Revocation stops the *next* connection; a session already open keeps working until it ends (T-0052) | `arreo audit export --action auth.reject` after the device reconnects |
| `device.issue`/`device.revoke` names a device I did not act from | Those rows record the *subject* device, not the actor | §3 — and for a revocation the actor is in `detail`, `revoked by <actor>` |
| A prompt is missing its second and later lines | The table prints the first line only | Use `--json` or `export`, which carry the whole prompt |
| The prompt shows `[redacted]` and I cannot tell what was masked | The plaintext was never written; redaction happens on the way in | §4.1 — there is no un-redact |
| A whole line reads `[REDACTED:secret]` | The scan flagged it and no masking rule claimed it | §4.1 — the fallback masks rather than storing the bytes |
| The size in the boot warning does not match `ls -l` | The number is the summed length of the stored text columns, not the file size | §7 |
| The warning never appeared and the log is big | It is emitted once per boot, at 100 MiB of stored text | Restart the daemon, or watch the log with `arreo audit --limit 1` |
| Rows I wanted are gone after a prune | The prune is permanent | §7 — there is no archive step |
| `audit.prune` rows keep accumulating | Each prune writes one and none of them are prunable | §7 — delete them by hand only if you mean to edit the log |

## 10. What is not here yet

- **No hash-chaining and no tamper evidence.** The log is append-only by API —
  no update, no delete-one — but nothing cryptographically binds one row to the
  next. A row edited directly in the SQLite file leaves no trace.
- **Actions attribute to device ids, not to roles.** A row says which device
  acted, not what that device was allowed to do; the role is a property of the
  device registry, not of the trail.
- **The relay's own audit trail is a separate database and a separate task
  (T-0053).** This document covers the machine's.
- **A session already open when a device is revoked keeps working until it
  ends** (T-0052). The revocation is durable and audited, but it takes effect on
  the next connection.
- **`audit.prune` removes rows permanently.** There is no archive step, no
  export-before-prune and no undo.
- **The masking rules are heuristics.** A token is recognized by its prefix, so
  a secret in a shape none of the patterns cover (a bare 40-character AWS secret
  with no `aws_secret` label, a password written as prose) is stored as sent. A
  line whose key part contains `secret`, `password` or `passwd` has everything
  after its first `=`/`:` replaced whether or not the value is a secret, and a
  value shorter than 8 characters is kept when the key only contains `key`.
  What is *not* true in this build: a flagged line is never stored unmasked.
- **`kill` and `resize` are not recorded.** They are absent from the audited
  verb set (§3), and unlike the reads their absence is not documented in the
  code as a decision.
- **`expired` is defined but not written by this build.** The outcome exists in
  the vocabulary for something the machine was holding running out of time; only
  `ok` and `refused` are written today.
- **The `device` column is the subject, not the actor** — consistently, on
  every action. For `device.issue` and `device.rotate` that means the actor is
  not recoverable from the row at all (§3); a revocation does name its actor, in
  `detail`.
