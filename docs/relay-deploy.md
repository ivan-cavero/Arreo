# Running an Arreo relay (v1)

> Operator's guide to `arreo-relay` v1: the AGPL-3.0, self-hostable binary that
> routes opaque envelopes between the devices of one account. The wire it speaks
> is `docs/relay-protocol.md`; this document is what to run, what it writes, and
> what to watch. Everything here is a property of the code that ships in
> `crates/arreo-relay/` — nothing is aspirational, and the sections at the end
> name the parts that are not there yet.

One sentence: a self-hosted relay is one process, one UDP port and one SQLite
file; it authenticates devices by certificate, moves bytes it cannot read, and
tells the sender what became of each one.

## 1. What `serve` does

`arreo-relay serve --listen ADDR --state-dir DIR [--pairing-tcp ADDR]
[--pairing-socket PATH] [--inbox-ttl-days DAYS] [--inbox-max-messages N]
[--inbox-max-mb N]`, in order:

1. Parses its flags. `--state-dir` is required — the router's SQLite file lives
   there and must survive a restart. `--listen` defaults to `127.0.0.1:8787`,
   and the three inbox flags default to 30 days, 10,000 messages and 64 MiB
   (§8).
2. Creates the state directory if it does not exist, opens
   `<state-dir>/relay.db` (creating and migrating it), and logs
   `arreo-relay: state <path>`.
3. Starts the pairing mailbox listeners, if asked for, on their own blocking
   threads. They are independent of the router: a device pairs before it has a
   certificate to route with.
4. Checks the inbox bounds and logs the ones it will enforce:
   `arreo-relay: inbox retention 30 day(s), bounds 10000 message(s) / 64 MiB per device`.
   This happens after the database is open and before the socket is bound, so a
   bad bound is refused with the state directory already created (§8).
5. Builds its async runtime, binds the QUIC endpoint on the listen address, and
   logs the address it actually bound:
   `arreo-relay: router on <bound> — <loopback note or warning>`.
6. Serves sessions until it is killed or hits a fatal error.

Notes for a service manager:

- **It runs in the foreground and never daemonizes.** There is no PID file, no
  `--daemon` flag and no config file; run it under systemd (or your supervisor)
  with the same flags you would type by hand.
- **It installs no signal handlers.** SIGINT/SIGTERM terminate the process and
  every live session with it; devices reconnect and re-handshake.
- **Exit codes:** `2` for a usage error (unknown flag, missing `--state-dir`, a
  `--listen` that is not `IP:PORT`, an `--inbox-*` value that is not a whole
  number, a TTL outside 1..=365 days, a zero message or MiB bound), `1` for a
  fatal runtime failure (the state directory cannot be created, the database
  cannot be opened, the port cannot be bound, the router stopped).
- **`--listen 127.0.0.1:0`** asks the OS for a free port; the log line reports
  the port actually bound, which is the only way a caller can learn it.
- **`--help` is handled by the bare form** (`arreo-relay --help`, which prints
  all three usages). `arreo-relay serve --help` and `arreo-relay account --help`
  are unknown-flag/usage errors, exit 2. The printed `serve` usage line names
  the routing and pairing flags only; the three inbox flags of §8 are accepted
  all the same.

## 2. The three doors

One binary, because a self-hosted relay is one thing to run (ROADMAP §3.4).

| Door | Command | What it is |
| --- | --- | --- |
| Router | `arreo-relay serve --listen 127.0.0.1:8787 --state-dir DIR` | The routing service. Needs the state directory; authenticates devices and moves envelopes |
| Pairing mailbox | `arreo-relay --pairing-socket PATH` and/or `arreo-relay --pairing-tcp HOST:PORT` | The T-0024 SPAKE2 mailbox, alone. Deliberately in-memory: it needs no state and keeps nothing, so a restart mid-pairing fails that pairing and the human re-runs `arreo pair` |
| Account registry | `arreo-relay account add --state-dir DIR --account ID --root-key HEX` | The operator's hand on the account registry — the one thing that must happen before any device can connect |

`serve` can carry the pairing listeners too (`--pairing-socket`/`--pairing-tcp`
beside the router flags), which is the usual single-process deployment.

## 3. The state directory and its schema

The state directory holds one file the relay owns: `relay.db`, plus SQLite's
`-wal` and `-shm` companions while WAL is active. The journal mode is WAL and
foreign keys are on; the schema is created by an idempotent migration, so a
restart migrates rather than recreates, and a v1 database gains the v2 and v3
additions in place.

`meta.schema_version` is **3** for this build. The tables:

| Table | Columns | Who writes it |
| --- | --- | --- |
| `meta` | `key`, `value` | the migration; holds `schema_version` |
| `account` | `account_id` (PK), `created_at_ms`, `root_key` | `account add`. `root_key` is 64 hex characters; it was added in v2, and a row whose `root_key` is NULL or unparseable is treated as an **unknown account** by the router |
| `relay_device` | `account_id`, `device_id`, `first_seen_ms`, `last_seen_ms`; PK `(account_id, device_id)` | the router, on every successful authentication. `first_seen_ms` is written once and `last_seen_ms` is refreshed — the raw material for presence (T-0031), which does not exist yet |
| `machine` | `machine_id` (PK), `account_id`, `name`, `name_key`, `presence`, `last_seen_ms`, `proto_version`, `tombstone_until_ms`, `name_conflict`; `UNIQUE(account_id, name_key)` | the machine directory (T-0043). The router does not touch it, and nothing derives the `presence` column yet |
| `inbox` | `device_id`, `seq`, `received_at_ms`, `expires_at_ms`, `bytes`; PK `(device_id, seq)`, with an index on `expires_at_ms` | the router, when a message is queued for a device that is not connected (T-0030, §8). `bytes` is the whole framed envelope exactly as its sender wrote it: the relay never decodes what it stores |
| `inbox_stats` | `device_id` (PK), `dropped_total`, `expired_total`, `bytes`, `queued`, `dropped_reported` | the router, in the same transaction as every inbox write. The durable per-device counters behind §8; `dropped_reported` is the watermark the last drain already reported |

Everything in this file is **metadata or opaque bytes**. The account id, the
public key, the device fingerprints and the timestamps are metadata; the
`inbox.bytes` column holds queued envelopes the relay cannot interpret — it
stores them, it does not read them, and confidentiality is the daemons' Noise
session (T-0023). A schema test asserts that the inbox row can hold nothing
else: `device_id`, `seq`, `received_at_ms`, `expires_at_ms` and one blob, and no
column whose name suggests a key, a pairing code or agent state. What the relay
routes *live* it still persists nowhere at all — the integration test sends a
marked pane-shaped payload to a connected device and asserts the marker appears
nowhere in the state directory or the logs. A queued message is the deliberate
exception: it is written down, as bytes the relay cannot read.

## 4. The default listen address, and why non-loopback is warned about

`--listen` defaults to **`127.0.0.1:8787`**. The shipped posture is a relay on
the operator's own machine or private network, not on the open internet.

A loopback listener logs:

```text
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)
```

Anything else logs a warning that says exactly what the risk is:

```text
— reachable off this machine: the relay authenticates each device by verifying its certificate
against the account's registered root key, and never reads what it routes, but anyone who can
reach this port can open a session and be refused
```

Read that carefully: the relay will not admit anyone who cannot prove possession
of a device key that chains to the account root, and it cannot read what it
routes — but the port is still reachable by anyone who can route to it, there is
no global cap on concurrent connections, and a peer that completes the QUIC
handshake and then stays silent holds a handshake task until it goes away.
Binding to a non-loopback address is a decision, so it is stated rather than
silently allowed. Prefer a private network (VPN or overlay) or a firewall that
admits only the addresses that need it.

## 5. Registering an account

An account's root public key is the anchor every device certificate in it is
verified against. Until an account is registered, **every** device that claims
it is refused with `unknown account <id>` — before any cryptography runs. The
pairing flow does not register accounts yet, so this command is the operator's
door (T-0029's landing notes).

Get the root public key on the server host, where the identity lives:

```console
$ arreo devices list --json | jq -r .root
4c1f8d3a...                # 64 hex characters: the ed25519 public half of the server's root key
```

(`arreo devices list` prints the same value truncated to a fingerprint; the
private half stays in the server's identity directory and is never needed here.
If the daemon's socket is not at the default location, pass it:
`arreo devices list --socket /run/arreo/arreo.sock --json`.)

Register it at the relay:

```console
$ arreo-relay account add --state-dir /srv/arreo-relay --account acct-1 \
    --root-key 4c1f8d3a...
registered account acct-1 with root key 4c1f8d3a...
```

- `--root-key` must be exactly 64 hex characters; anything else is a usage error
  (exit 2). The account id itself is an opaque string — the relay stores it and
  compares it, and does not parse it.
- **Re-running the command for the same account replaces its root key.** That
  is deliberate (re-keying), and it has teeth: every device certificate issued
  under the old root stops verifying immediately.
- **It works while `serve` is running.** The router reads the root key from
  SQLite on each `Hello`, so a newly registered account is usable at once; there
  is no reload signal and no restart needed. WAL lets the two processes share
  the file.

## 6. The handshake budget

The router counts incoming connections **per source IP address: 3 in any
10-second window**. The 4th connection in the window is dropped at accept time,
before any QUIC or application work, and **nothing is logged for it**.

- A **successful authentication clears that address's history**, so a device
  that reconnects after a real network drop is not punished for the retries that
  got it there. Refusals deliberately do not forgive — a peer that keeps failing
  is exactly who the budget is for.
- It is per process and per address, not global, and it resets when the relay
  restarts.
- Operationally: if you drive several clients from one host (a test box, a
  script that retries), you can trip your own budget and see connections
  disappear with no log line. Three failed handshakes in ten seconds is the
  whole allowance.

## 7. Restart behavior

| Survives a restart | Does not survive |
| --- | --- |
| the account registry (`account`), including root keys | live sessions — the `(account, device)` map is in memory, so every device must reconnect and re-handshake |
| the device registry (`relay_device`), first/last seen | the per-address handshake budget |
| the machine directory rows (`machine`) and the schema version | the pairing mailbox, which is in-memory by design (a restart mid-pairing fails that pairing) |
| the queued messages (`inbox`) and their counters (`inbox_stats`), until their TTL or a bound takes them | the envelope being routed at the instant of the crash: it was never committed to an inbox, and the sender learns nothing |

A restart is therefore not a data-loss event for anything the relay *knows* or
has *queued*: the durable part is the accounts, the devices, the directory and
the inbox. What it does drop is the work in flight — an envelope being routed
when the process dies is gone, and the sender learns nothing, because delivery
reports live only as long as the session. Since T-0030, that exposure is one
envelope: everything already answered `queued` is on disk, and the destination
gets it when it next drains (§8).

## 8. The inbox: bounds, counters, and a device that stopped draining

Since T-0030 the relay keeps a durable queue per destination device. It is the
one part of the relay that grows with traffic, and the part an operator is most
likely to have to reason about, so it gets its own section. The wire — `drain`,
`ack`, the drain report, and the at-least-once contract — is
`docs/relay-protocol.md` §4.4 and §4.5.

### 8.1 The three bounds, and where they are set

| Flag | Default | Accepted | What it bounds |
| --- | --- | --- | --- |
| `--inbox-ttl-days` | `30` | 1..=365 days | how long a message may wait before it expires |
| `--inbox-max-messages` | `10000` | at least 1 | how many messages one device may have queued |
| `--inbox-max-mb` | `64` | at least 1 | how many MiB one device may have queued |

All three are **per destination device**, not per relay: two hundred devices
with full inboxes is two hundred times `--inbox-max-mb`. They are read once at
startup, so changing one means restarting `serve` — there is no reload — and a
value outside its range is a usage error (exit 2). Because the check happens
after the database is opened, a bad bound still leaves the state directory
created.

Startup logs the values in force, before the socket is bound:

```text
arreo-relay: inbox retention 30 day(s), bounds 10000 message(s) / 64 MiB per device
```

### 8.2 Reading the queue: depth, and the two kinds of counter

Two facts decide which number to trust. The **row count is always the truth
about depth**; the cached columns in `inbox_stats` can be stale, because a sweep
deletes rows without recomputing them — and a sweep triggered by one device's
enqueue can expire another device's rows. So read depth from `inbox`:

```console
$ sqlite3 /srv/arreo-relay/relay.db \
    "SELECT device_id, COUNT(*), SUM(LENGTH(bytes))
       FROM inbox GROUP BY device_id ORDER BY 2 DESC;"
dev_4b1e...|3|1024
```

and the counters from `inbox_stats`:

```console
$ sqlite3 /srv/arreo-relay/relay.db "SELECT * FROM inbox_stats;"
dev_4b1e...|1|0|1024|3|0
```

| Column | What it means |
| --- | --- |
| `dropped_total` | lifetime drops for that device: evictions forced by a bound, plus expiries. A drop the operator cannot count is a drop that never happened, so this one is durable |
| `expired_total` | the part of `dropped_total` that was expiry rather than eviction; `dropped_total - expired_total` is what the bounds forced |
| `dropped_reported` | the `dropped_total` watermark the device's last drain already carried. `dropped_total - dropped_reported` is what its *next* drain will report — and because the watermark moves when the drain runs, not when the report arrives, a device that disconnected mid-drain has been counted but not told |
| `bytes`, `queued` | cached copies of the depth, written on enqueue, drain and ack — and not on a sweep, which is why the depth above is counted from the rows |

These are read-only queries. The relay owns the file; do not write to `relay.db`
by hand while `serve` is running, and there is no `arreo-relay` command that
inspects or clears an inbox yet.

### 8.3 The hourly sweep

Expiry happens in two places: lazily, inside every enqueue and every drain; and
once an hour, on the relay's own timer, over the whole store — so a device that
never comes back cannot keep the disk full. The hourly pass logs only when it
did something:

```text
arreo-relay: inbox sweep expired 3 message(s)
```

`0` is silent. A sweep that fails logs
`arreo-relay: inbox sweep failed: <error>` and the router keeps serving; expired
rows then wait for the next enqueue or drain to be reclaimed.

### 8.4 A device that has not drained for a long time

The queue is bounded, so the failure mode is not unbounded growth — it is
*eviction*, and eviction is where messages are lost.

- The queue fills to `--inbox-max-messages` or `--inbox-max-mb`, and from then
  on every new message evicts the oldest one for that device. The **sender** is
  not told: it was answered `queued` with the depth at the time. What changes is
  the number in `queued` on each new status, so a sender watching that climb is
  watching the bound approach.
- The **destination** is told at its next drain: the drain report carries
  `dropped` (drops since the last drain) and `expired`, and
  `dropped_total`/`expired_total` keep the lifetime count for you (§8.2).
- A device that never returns has everything expire after `--inbox-ttl-days`,
  and the hourly sweep deletes the rows. Nothing needs doing by hand: the
  retention window is what guarantees a dead device's queue is reclaimed.

What to do when a queue is full or a device has stopped draining:

- **Check whether the device is even reachable.** `last_seen_ms` in
  `relay_device` is the last successful authentication, and it is the only
  signal there is: presence does not exist yet (T-0031), so there is no "last
  seen 2 days ago" answer and no way to tell a sleeping device from a gone one.
  A device that fails to connect is refused at the handshake (§5), and that
  refusal is logged with its reason.
- **Raise the bound, or accept the eviction.** `--inbox-max-messages` and
  `--inbox-max-mb` are the lever, they are per device, and raising them needs a
  restart. Raising them does not bring back anything already evicted.
- **The TTL is written per row, at enqueue time.** `--inbox-ttl-days` is applied
  when a message is queued, so a row already on disk keeps the expiry it was
  written with and changing the flag affects new messages only. There is no
  command to drop a device's queued rows early.
- **A full inbox is not a relay failure.** The bounds are per device, the sweep
  is global, and the relay keeps serving every other device throughout: a
  stalled queue is a fact about one destination, not an outage.

## 9. Deployment shapes

### 9.1 Direct QUIC on the host

```console
$ arreo-relay serve --listen 0.0.0.0:8787 --state-dir /srv/arreo-relay
```

The relay binds QUIC on UDP 8787 and logs the non-loopback warning of §4.
Sensible when the host sits on a private network (a VPS with a restricted
security group, a LAN, a VPN interface). The authentication is the device
certificate, not the network — but restrict who can reach the port anyway,
because reachability is the only thing the network gives you.

### 9.2 Loopback plus a UDP forward (the shipped posture)

```console
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay
```

and forward UDP port 8787 to it from whatever the devices can reach: a
WireGuard/Tailscale overlay, or a forwarder such as

```console
$ socat UDP4-LISTEN:8787,fork UDP4:127.0.0.1:8787
```

**It must be a UDP forward.** QUIC runs over UDP, so a TCP forward (`ssh -L`,
an HTTP reverse proxy, a TCP load balancer) does not carry this protocol at all;
it will accept connections and never move an envelope. This is the shape to
prefer: the relay itself is only reachable from the host, and the network in
front of it is the thing you already know how to secure.

### 9.3 The TCP/WebSocket fallback behind a TLS terminator — not shipped

**This path does not exist in v1.** The routing path is QUIC/UDP only: there is
no TCP listener, no WebSocket upgrade and no HTTP surface in the router. A
network that blocks UDP cannot be worked around by putting a TLS terminator in
front of the relay; today the options are a UDP-capable tunnel (§9.2) or a
different network. The fallback remains a documented direction, not a feature,
and nothing in this binary will answer on a TCP port.

## 10. A worked local example

No secrets are involved: the root key hex is public material.

```console
# 1. On the server host: the account's root public key, 64 hex characters.
$ arreo devices list --json | jq -r .root
4c1f8d3a9b0e77c2...

# 2. Register that account at the relay (creates the state dir and relay.db).
$ mkdir -p /srv/arreo-relay
$ arreo-relay account add --state-dir /srv/arreo-relay --account acct-1 \
    --root-key 4c1f8d3a9b0e77c2...
registered account acct-1 with root key 4c1f8d3a9b0e77c2...

# 3. Run the router, with the inbox defaults in force.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: inbox retention 30 day(s), bounds 10000 message(s) / 64 MiB per device
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)

# 4. Optional: the pairing mailbox beside the router, for devices that do not
#    have a certificate yet.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay \
    --pairing-tcp 127.0.0.1:8770
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: pairing mailbox on tcp://127.0.0.1:8770
arreo-relay: inbox retention 30 day(s), bounds 10000 message(s) / 64 MiB per device
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)

# 5. Tighten the inbox bounds for a small host: one week of retention, at most
#    2,000 messages or 16 MiB per device.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay \
    --inbox-ttl-days 7 --inbox-max-messages 2000 --inbox-max-mb 16
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: inbox retention 7 day(s), bounds 2000 message(s) / 16 MiB per device
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)
```

The line to wait for in a script is `router on <addr>` on stderr: it is printed
after the socket is bound, and it reports the port actually bound, so
`--listen 127.0.0.1:0` is usable in tests and ephemeral deployments.

A client then connects over QUIC to that address, sends `Hello` for `acct-1`,
completes the nonce handshake and starts exchanging envelopes — the exact
messages are in `docs/relay-protocol.md`. A device whose account was never
registered is refused with `unknown account acct-1`, which is the single most
common first-run mistake.

A device that is *known* but not connected is no longer refused: a `frame` for
it is committed to its inbox and the sender is answered `queued`. When the
device connects and drains (protocol §4.4), it gets the queued envelopes and
then the drain report, and its `ack` removes them. A device that has never
authenticated in the account is still `no_such_device`, and a message larger
than that device's whole byte budget is refused rather than queued (§8.4).

## 11. What to watch in the logs

Everything the relay says goes to **stderr**, one line per event, prefixed
`arreo-relay:`. There is no log level and no file destination — ship stderr to
your collector if you want to keep it.

| Line | What it means |
| --- | --- |
| `state <path>` | the database in use |
| `router on <addr> — loopback only (<addr>)` | bound and serving on loopback |
| `router on <addr> — <addr> — reachable off this machine: …` | bound on a non-loopback address; see §4 |
| `pairing mailbox on unix://<path>` / `tcp://<addr>` | a pairing listener started |
| `<peer> authenticated as dev_<hex> in account <id>` | a session came up; the device is routable and its `last_seen_ms` was refreshed |
| `refused <peer>: unknown account <id>` | a device claimed an account nobody registered — run `account add` (§5) |
| `refused <peer> for account <id>: <reason>` | the handshake failed at the certificate/proof stage; `<reason>` is the same string the client was sent |
| `refused envelope from dev_<hex>: <reason>` | an envelope was rejected: a spoofed sender, a foreign account, a malformed id, or a device trying to originate a status |
| `<device> is not reading its delivery reports; dropping one` | a client stopped reading its own stream; it is now missing delivery reports, so it cannot tell what was delivered |
| `inbox retention <d> day(s), bounds <m> message(s) / <k> MiB per device` | the inbox bounds this process enforces; printed at startup, before the socket is bound (§8.1) |
| `inbox sweep expired <n> message(s)` | the hourly sweep deleted `n` rows past their TTL; printed only when `n` is not 0 (§8.3) |
| `inbox sweep failed: <error>` | the sweep could not run; the router keeps serving, but expired rows are not being reclaimed |
| `cannot queue for <device>: <error>; refusing instead of dropping silently` | a queued write failed — typically a message larger than that device's whole byte budget — so the sender was answered `refused` instead of `queued` (§8.4) |
| `<device> is not reading its drain; stopping this batch` | a device stopped reading mid-drain; the rest of that batch and its report were not delivered, and the device must drain again |
| `<device> disconnected (clean)` / `(error)` | a session ended |
| `session ended: <error>` | a session ended on a protocol or transport error — a malformed frame, an unsupported version, a broken connection |
| `cannot create <dir>: …`, `cannot open <path>: …`, `cannot listen on <addr>: …`, `router stopped: …` | fatal; the process exits 1 |
| `unknown flag …`, `serve needs --state-dir …`, `--listen "…" is not an IP:PORT address: …`, `--inbox-ttl-days needs a positive whole number`, `inbox TTL must be 1..=365 days, got <n>`, `inbox message bound must be at least 1`, `inbox byte bound must be at least 1 MiB`, `account add needs …`, `--root-key must be 64 hex characters (an ed25519 public key)` | usage error; exit 2 |

Two silences worth knowing: a rate-limited connection (§6) produces **no log
line at all**, and neither a `delivered` nor a `queued` outcome is logged —
delivery is reported to the sender on its own stream, not to the operator. The
per-device counters in `inbox_stats` are the operator's record of the queue
(§8.2).

## 12. What v1 does not do, in the operator's terms

These are real gaps, not configuration:

- **The queue is at-least-once, and the consumer's half is not optional.** A
  queued message is redelivered until the device acks it, so a device that
  disconnects mid-drain — or simply drains twice — sees the same message again
  unless it de-duplicates on the frame's own `(src_device, seq)`. Nothing in the
  relay enforces that; the contract is `docs/relay-protocol.md` §4.5.
- **A connected device that stops reading is not queued.** The inbox is for a
  destination that is *not connected*. If the destination has a live session
  whose outbound queue is full, the sender is answered `offline` and the
  envelope is dropped — it is not committed to the inbox. A sender that needs
  that case covered must retry.
- **No push wakeup.** A queued message waits for the device to reconnect and
  drain. The relay cannot wake a sleeping phone, and wiring the daemon side to
  the relay is T-0050.
- **A queue is not a promise.** Inside the bounds a message waits for its TTL; a
  full queue evicts oldest-first, and a message past the TTL expires. Both are
  counted (`dropped_total`/`expired_total`) and reported on the next drain, but
  the message is gone (§8.4).
- **No presence yet.** The relay records `first_seen_ms`/`last_seen_ms` per
  device, but nothing derives online/offline from it, and there is no "last seen
  2 days ago" answer — so a stalled inbox cannot be told from a device that is
  merely asleep. Presence is T-0031.
- **Refusals live only in stderr.** There is no durable audit table to query
  yet (T-0033), so anything you did not capture is gone.
- **No revocation propagation.** The relay verifies the certificate chain and
  the proof of possession, but it has no revocation list (T-0026): revoking a
  device on the server does not stop the relay from admitting its certificate.
  The crude workaround today is re-registering the account with a new root key,
  which invalidates *every* device in it — not a substitute for revocation.
- **The relay does not encrypt.** It carries payload bytes without reading them,
  and while a message is queued it keeps those bytes in `relay.db` for up to the
  retention window, but it does not make them unreadable. End-to-end
  confidentiality is the daemons' Noise session (T-0023) and wiring the daemon
  side to the relay is T-0050; a client that sends plaintext gives the relay
  plaintext.
- **QUIC/UDP only** (§9.3), and there is no global connection cap.
