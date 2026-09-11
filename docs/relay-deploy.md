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

## 12. The daemon's side: dialling the relay

Everything above is the relay. This section is the other half: what
`arreo-server` — the daemon that owns the panes — does with a relay, what it
needs before it will dial, and what it says while doing it. The code is
`crates/arreo-server/src/relay_client.rs` (the session) and the `[relay]` wiring
in `crates/arreo-server/src/main.rs`.

One sentence: the daemon dials out, authenticates with the device certificate it
already holds, and then serves protocol sessions to peers over the relay exactly
as it serves them over its local socket — no inbound port, no second identity,
and no change to the local API.

### 12.1 What the daemon does with the relay

Five things, on every session, in order:

1. **It dials out.** `RelaySession::dial(addr, account, device_key, cert)`
   connects to the relay at `addr` and registers this device for `account`.
   Nothing listens on the daemon's side: the shipped posture is zero inbound
   ports, and the relay's address is the only thing it needs to be able to
   reach.
2. **It authenticates with its own device certificate.** The identity is the
   machine's own — `identity/device.key` plus `identity/devices/<id>.cert`, the
   pair `arreo pair` left behind — so the relay authenticates the identity the
   machine already presents, with no second key and no second pin. The
   certificate's file name is the **bare** hex device id
   (`identity/devices/<32 hex>.cert`), not the `dev_`-prefixed form used in logs
   and on the command line.
3. **It drains what was queued.** On connect the session drains its inbox from
   the start, so "the machine was off" and "the machine is on" are the same path
   (T-0030, §8): everything the relay queued while the daemon was away arrives
   here.
4. **It accepts peers.** A peer that connects to this daemon through the relay
   gets a byte stream, and the daemon hands that stream to the **same**
   `serve_session` loop the local socket runs, behind the same per-verb gate:
   `DeviceAuthority::check_verb` decides each verb from the peer's role. A relay
   peer therefore has exactly the permissions it would have locally — a viewer
   cannot send, and a device that is not pinned never gets past the handshake.
5. **It probes the configured peer.** If `peer` is set, the daemon opens a
   session to it and asks one question — `Hello`/`Welcome`, then `Panes` — and
   logs the count the peer reports.

The probe is deliberately read-only: it asks for the pane *list* and attaches to
nothing, so a boot-time probe cannot change the peer's machine. It runs **once
per relay session**, not once per peer connection, so a reconnect probes again.

### 12.2 The configuration file

The daemon's flags are `arreo-server [--socket PATH] [--config PATH]`, and
`--config` may be replaced by the `$ARREO_CONFIG` environment variable. If both
are set, `--config` wins. **With neither, the relay is off and nothing about it
is logged**: a self-hosted runtime must work with no relay at all.

The file is TOML, and only the `[relay]` section is read:

```toml
[relay]
enabled = true
addr = "10.0.0.1:8787"
account = "acct-1"
peer = "dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138"
```

| Key | Type | Default | Rules |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | the only switch. `false`, a file with no `[relay]` section, and a file that does not exist are all "no relay", and all silent |
| `addr` | string, `IP:PORT` | none | **required when `enabled = true`**; must parse as a socket address (`10.0.0.1:8787`, `[::1]:8787`) |
| `account` | string | none | **required when `enabled = true`**; must not be blank, and is sent to the relay verbatim |
| `peer` | string, device id | none | optional; a device id in either spelling (`dev_<hex>` or the bare `<hex>`) — the machine this daemon opens a session to and probes |

A file that *enables* the relay but is incomplete is a **loud exit**, not a
silent no-op: an operator who asked for the remote path must not quietly fail to
get it. The process exits 1 with

```text
arreo-server: relay configuration is unusable: the [relay] section of /etc/arreo/relay.toml is incomplete: [relay] enabled without `addr`
```

The four incomplete reasons, verbatim:

```text
[relay] enabled without `addr`
`addr` is not an IP:PORT address: <error>
[relay] enabled without `account`
`peer` is not a device id: <error>
```

An unreadable or unparseable file is refused the same way, with the same exit
code, prefixed `cannot read <path>: ` or `cannot parse <path>: `.

Two things that are easy to get wrong:

- **A `--config` path that does not exist is "no relay", silently.** A missing
  file is one of the three "off" cases, so a typo in the path looks exactly like
  a relay that never dials: nothing is logged, and nothing is dialled.
- **Unknown keys are ignored.** Neither the file nor the section rejects extra
  keys, so `enable = true` (a typo for `enabled`) leaves the relay off without a
  word.

### 12.3 The logs, step by step

The daemon writes everything to **stderr**, one line per event, prefixed
`arreo-server:`. Nothing about the relay is written while the relay is off.

At startup, once the configuration has been accepted and the relay task started:

```text
arreo-server: relay enabled for account acct-1 via 10.0.0.1:8787
```

The dial itself is silent — there is no "dialling" line, and an attempt in
progress says nothing. Every attempt then ends in exactly one outcome line:

```text
arreo-server: relay session up as dev_1a2b3c4d5e6f708192a3b4c5d6e7f809 (account acct-1, relay 10.0.0.1:8787)
arreo-server: relay registration failed (10.0.0.1:8787): the relay refused the session: unknown account acct-1
```

When a peer authenticates to this daemon, and when the probe gets its answer:

```text
arreo-server: relay peer dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 authenticated
arreo-server: relay peer dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 reports 1 pane(s)
```

And every attempt — successful or not — is followed by its retry delay, after a
session that ended has said so:

```text
arreo-server: relay session ended; reconnecting
arreo-server: retrying the relay in 250ms
```

The full set:

| Line | What it means |
| --- | --- |
| `relay enabled for account <id> via <addr>` | the configuration was accepted; printed once, at boot, before the task starts |
| `relay session up as dev_<hex> (account <id>, relay <addr>)` | the relay accepted this device's certificate and the session is live |
| `relay session ended; reconnecting` | the session stopped; a reconnect follows |
| `relay registration failed (<addr>): <error>` | the attempt failed. `<error>` is the relay's own reason (`the relay refused the session: <reason>`), a transport failure, or a timeout |
| `retrying the relay in <delay>` | the delay before the next attempt (§12.4) |
| `relay peer dev_<hex> authenticated` | a peer completed the handshake and is pinned; its session now runs the local protocol, gated per verb |
| `relay peer dev_<hex> refused: <error>` | the peer's handshake failed — most often `the peer announced an identity that is not pinned` |
| `relay peer dev_<hex> is not pinned; refusing` | the post-handshake re-check failed |
| `relay peer dev_<hex> session error: <error>` | the peer's protocol session ended with an error |
| `relay peer dev_<hex> reports <n> pane(s)` | the probe's answer |
| `cannot reach dev_<hex> through the relay: <error>` | the probe could not open a session — typically `the relay could not deliver: the peer is offline` |
| `cannot probe dev_<hex>: it is not pinned on this machine` | the configured `peer` is not pinned here, so there is nothing to authenticate it against |
| `relay peer dev_<hex> did not answer: <error>` / `… did not answer within 15s` | the peer accepted the session but the pane list did not come back in time |
| `cannot drain the relay inbox: <error>` | the post-connect drain failed; the session carries on |
| `relay inbox reported <n> dropped and <m> expired message(s) since the last drain` | the relay dropped or expired queued messages for this device (§8.4); printed only when either is non-zero |
| `relay write failed: <error>` | the outbound half of the session died |
| `relay session ended: <error>` | the inbound half died — a malformed envelope, an unsupported version, a broken connection |
| `daemon: refusing <Verb> for dev_<hex>: <reason>` | a relay peer asked for a verb its role does not hold; the same gate, and the same line, as a local client |

And the boot lines around it, which decide whether the relay starts at all:

| Line | What it means |
| --- | --- |
| `device authority ready (root <hex>…, <n> device(s))` | the authority loaded; the relay needs it |
| `serving on <path>` | the local socket is up; the relay dials beside it, not instead of it |
| `device identity unavailable: <error>` / `refusing to serve without a device authority (…)` | fatal, exit 1 — and it happens before the relay is even considered |
| `relay enabled but this machine has no identity: <error>` / `pair this machine first (arreo pair), or set enabled = false` | fatal, exit 1: the relay was asked for but `identity/device.key` or its certificate is missing |
| `relay configuration is unusable: <error>` | fatal, exit 1: the configuration enables the relay but cannot be used |
| `unknown flag <flag>` | a usage error, exit 2 |

### 12.4 The reconnect policy

The loop is: dial, serve, and on **any** ending wait, then try again. The wait
is exponential with a ceiling, plus jitter:

| Policy | Value |
| --- | --- |
| base | `250 ms` |
| growth | doubled per attempt |
| ceiling | `30 s` |
| jitter | up to **25%**, added *after* the ceiling is applied |

So the delays are 250 ms, 500 ms, 1 s, 2 s, 4 s, 8 s, 16 s, 30 s, 30 s, …, and
the printed value is the jittered one — at the ceiling that is up to about
37.5 s, so `retrying the relay in 37.4s` is not a bug. The attempt counter
resets to zero as soon as a session comes up.

Two properties worth knowing:

- **A *refused* registration is retried on the same schedule, not tightly.** A
  bad certificate or an unregistered account will not fix itself by being
  presented again sooner, so the refusal is logged with the relay's own reason
  and the daemon backs off exactly as it would for an absent relay. This is also
  why a misconfigured account does not hammer the relay.
- **The local socket is unaffected throughout.** The relay is a task beside the
  daemon: dialling, failing, retrying and reconnecting never stop the local
  socket from serving, and the acceptance tests assert exactly that — a relay
  that cannot be reached leaves the local API working and the reason in the log.

A reconnect is a **new session**, not a resumed one. A new stream to the peer
and a new Noise handshake follow, and nothing queued in the previous session
resumes: the write path logs `relay write failed`, a stream's reader gets the
recorded reason as an error rather than a silent gap, and the session's closure
starts the next attempt. What the *relay* queued for this device is the
exception, and it is drained at the start of the new session (§12.1).

### 12.5 A worked two-machine example

Two machines in one account (`acct-1`), one relay. Machine **A** probes; machine
**B** serves. Both must be paired — each needs `identity/devices/<id>.cert`, and
an unpaired machine is a loud exit (§12.2) — and each must have the other
pinned, because a relay peer is authenticated by the same authority that gates
the local socket.

```console
# 1. On the machine that holds the account identity (B, the one that pairs the
#    others): the account's root public key, for the relay's registration (§5).
$ arreo devices list --json | jq -r .root
4c1f8d3a9b0e77c2...

# 2. On the relay host: run the router, then register the account.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: inbox retention 30 day(s), bounds 10000 message(s) / 64 MiB per device
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)

$ arreo-relay account add --state-dir /srv/arreo-relay --account acct-1 \
    --root-key 4c1f8d3a9b0e77c2...
registered account acct-1 with root key 4c1f8d3a9b0e77c2...

# 3. On B: its own device id, which A must pin.
$ arreo devices id
device dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 key b41e...
(key file: /home/dev/.local/share/arreo/identity/device.key)
pin it on the server with: arreo devices issue --name <name> --role <owner|viewer> --key b41e...

# 4. On A: pin B. A's own certificate came from pairing, which pinned A on B,
#    so this is the one direction left to do by hand.
$ arreo devices issue --socket /run/arreo/arreo.sock \
    --name machine-b --role owner --key b41e...
issued dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 (machine-b) for owner as owner — serial 1
```

```console
# 5. A's configuration: it probes B, so it names B.
$ cat /etc/arreo/relay.toml
[relay]
enabled = true
addr = "10.0.0.1:8787"
account = "acct-1"
peer = "dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138"

# 6. B's configuration: it only serves, so it has no `peer`.
$ cat /etc/arreo/relay.toml
[relay]
enabled = true
addr = "10.0.0.1:8787"
account = "acct-1"
```

```console
# 7. Start B first: it must already be connected when A's probe runs, because
#    the relay routes only to a device with a live session.
$ arreo-server --socket /run/arreo/arreo.sock --config /etc/arreo/relay.toml
arreo-server: device authority ready (root 4c1f8d3a9b0e77c2…, 1 device(s))
arreo-server: relay enabled for account acct-1 via 10.0.0.1:8787
arreo-server: serving on /run/arreo/arreo.sock
arreo-server: relay session up as dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 (account acct-1, relay 10.0.0.1:8787)

# 8. Give B something to report, so the probe's answer is not a zero.
$ arreo spawn build /bin/sh -c "sleep 600" --socket /run/arreo/arreo.sock

# 9. Start A.
$ arreo-server --socket /run/arreo/arreo.sock --config /etc/arreo/relay.toml
arreo-server: device authority ready (root 8d2a4e6f0b1c3d5a…, 1 device(s))
arreo-server: relay enabled for account acct-1 via 10.0.0.1:8787
arreo-server: serving on /run/arreo/arreo.sock
arreo-server: relay session up as dev_1a2b3c4d5e6f708192a3b4c5d6e7f809 (account acct-1, relay 10.0.0.1:8787)
arreo-server: relay peer dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 reports 1 pane(s)
```

The line to wait for on A is `relay peer dev_<hex> reports <n> pane(s)`: it means
the relay carried a session, the Noise handshake completed over it, and B
answered the daemon protocol. On B the matching line is
`relay peer dev_<hex> authenticated`. On the relay's own stderr you see the other
half — a session per machine, and no payload:

```text
arreo-relay: 127.0.0.1:53412 authenticated as dev_1a2b3c4d5e6f708192a3b4c5d6e7f809 in account acct-1
arreo-relay: 127.0.0.1:53414 authenticated as dev_9f2c4a1b7d3e50618c4f2a9b6d0e7138 in account acct-1
```

Two notes on the example:

- **A's `root` is not the account root.** The `root` in the boot line is *this
  machine's* authority root, and a machine that was paired — rather than the one
  that did the pairing — has a root of its own: `arreo devices issue` signs the
  peer's certificate with that root, while the relay only ever verifies the
  certificate chain of the account registered at it. Pinning a peer locally and
  registering an account at the relay are two different acts.
- **Start the serving machine first.** If A's probe runs before B is connected,
  the relay answers `offline` and A logs
  `cannot reach dev_9f2c… through the relay: … the peer is offline`. Because the
  probe runs once per session, it is not retried until A's own relay session
  ends and is re-established — so either start B first, or restart A after B is
  up.

### 12.6 Troubleshooting

| Symptom | Likely cause | What to check |
| --- | --- | --- |
| exit 1, `relay configuration is unusable: … incomplete: …` | the `[relay]` section enables the relay but is missing a required key | `addr` and `account` are both required when `enabled = true` (§12.2) |
| exit 1, `relay enabled but this machine has no identity: cannot read …` | the machine is not paired: no `identity/device.key`, or no certificate under `identity/devices/` | pair this machine (`arreo pair`), or set `enabled = false` |
| `relay registration failed (<addr>): the relay refused the session: unknown account <id>` | the account is not registered at the relay, or the root key there is not the one that signed this machine's certificate | `arreo-relay account add --state-dir … --account <id> --root-key <hex>` (§5); re-registering replaces the key and invalidates every device under the old one |
| `relay registration failed (<addr>): …` repeating, with a growing `retrying the relay in …` | the relay is unreachable — not running, wrong address, or UDP is blocked | the relay's `router on <addr>` line (§10), the `addr` in the config, and UDP reachability between the hosts (§9.2) |
| `relay peer dev_<hex> refused: the peer announced an identity that is not pinned` | the connecting peer is not pinned on this machine | `arreo devices list --socket …` here, and pin the peer's key with `arreo devices issue … --key <hex>` |
| `cannot probe dev_<hex>: it is not pinned on this machine` | the configured `peer` is not pinned here | pin the peer before starting the daemon, as in §12.5 |
| `cannot reach dev_<hex> through the relay: … the peer is offline` | the peer is not connected to the relay — the relay routes only to a live session | start the peer's daemon with its own `[relay]` section; a machine that only serves must still be connected |
| `relay peer dev_<hex> did not answer within 15s` | the peer's session was accepted but the pane list did not arrive | the peer's own log: did it authenticate this machine, and is it still serving? |
| `relay peer dev_<hex> reports 0 pane(s)` | not a failure: the peer has no panes | the probe asks for the list only and attaches to nothing (§12.1) |
| nothing about the relay in the log at all | the relay is off — no `--config`, no `$ARREO_CONFIG`, a missing file, no `[relay]` section, `enabled = false`, or a misspelled key | the config file actually named by `--config`/`$ARREO_CONFIG`, and its `enabled` spelling (§12.2) |
| `daemon: refusing Send for dev_<hex>: …` on the serving machine | the peer's role does not hold that verb | the role the peer was pinned with (`--role owner` or `--role viewer`) |

### 12.7 What the daemon's relay leg does not do

- **The probe is a pane *count*, not an attach.** It asks `Panes` and logs the
  number; it does not open a pane, stream its output or send input. Remote
  attach is T-0032.
- **The probe runs once per relay session.** A probe that fails — the peer
  offline, the peer silent — is not retried until the relay session ends and is
  re-established, and there is no command that asks for it again.
- **The relay leg carries protocol sessions, not per-pane bytes.** A peer that
  reaches this daemon gets the daemon's own socket API over the relay stream,
  and every verb is gated by the peer's role. Nothing here forwards a pane's raw
  output to a peer that has not been granted the verb for it.
- **A peer must be pinned on this machine.** The handshake resolves the peer's
  announced id through the authority's index — the same door the per-verb gate
  uses, so the two agree — and an id that resolves to nothing is refused before
  any cryptography runs. The supported way to pin a peer is `arreo devices
  issue` (or pairing): it writes the certificate *and* the store row, and the
  store row is what carries the facts a certificate file does not — revocation
  and retirement. A certificate file written by hand is a state no product
  command produces; the file is read, but the durable record is missing.
- **This machine must be paired.** The relay authenticates it with
  `identity/devices/<id>.cert`, so a machine without one is a loud exit rather
  than a machine that quietly has no remote path (§12.2).
- **A reconnect loses the Noise session.** A new stream and a new handshake
  follow, and anything queued in the previous session fails loudly instead of
  resuming (§12.4). The relay's inbox is the part that does carry over, and it
  is drained at the start of the new session.
- **`peer` is optional, but serving still needs a connection.** A machine that
  only serves its peers needs no `peer` — but it must still be connected to the
  relay, because the relay routes only to a device with a live session. There is
  no push wakeup: a machine that is not connected cannot be reached through the
  relay at all.

## 13. What v1 does not do, in the operator's terms

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
  drain. The relay cannot wake a sleeping phone, and the daemon drains when it
  reconnects (§12).
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
  confidentiality is the daemons' Noise session (T-0023), which the daemon now
  runs over the relay (§12); a client that sends plaintext gives the relay
  plaintext.
- **QUIC/UDP only** (§9.3), and there is no global connection cap.
