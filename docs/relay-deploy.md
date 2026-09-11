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
[--pairing-socket PATH]`, in order:

1. Parses its flags. `--state-dir` is required — the router's SQLite file lives
   there and must survive a restart. `--listen` defaults to `127.0.0.1:8787`.
2. Creates the state directory if it does not exist, opens
   `<state-dir>/relay.db` (creating and migrating it), and logs
   `arreo-relay: state <path>`.
3. Starts the pairing mailbox listeners, if asked for, on their own blocking
   threads. They are independent of the router: a device pairs before it has a
   certificate to route with.
4. Builds its async runtime, binds the QUIC endpoint on the listen address, and
   logs the address it actually bound:
   `arreo-relay: router on <bound> — <loopback note or warning>`.
5. Serves sessions until it is killed or hits a fatal error.

Notes for a service manager:

- **It runs in the foreground and never daemonizes.** There is no PID file, no
  `--daemon` flag and no config file; run it under systemd (or your supervisor)
  with the same flags you would type by hand.
- **It installs no signal handlers.** SIGINT/SIGTERM terminate the process and
  every live session with it; devices reconnect and re-handshake.
- **Exit codes:** `2` for a usage error (unknown flag, missing `--state-dir`, a
  `--listen` that is not `IP:PORT`), `1` for a fatal runtime failure (the state
  directory cannot be created, the database cannot be opened, the port cannot be
  bound, the router stopped).
- **`--listen 127.0.0.1:0`** asks the OS for a free port; the log line reports
  the port actually bound, which is the only way a caller can learn it.
- **`--help` is handled by the bare form** (`arreo-relay --help`, which prints
  all three usages). `arreo-relay serve --help` and `arreo-relay account --help`
  are unknown-flag/usage errors, exit 2.

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
restart migrates rather than recreates, and a v1 database gains the v2 additions
in place.

`meta.schema_version` is **2** for this build. The tables:

| Table | Columns | Who writes it |
| --- | --- | --- |
| `meta` | `key`, `value` | the migration; holds `schema_version` |
| `account` | `account_id` (PK), `created_at_ms`, `root_key` | `account add`. `root_key` is 64 hex characters; it was added in v2, and a row whose `root_key` is NULL or unparseable is treated as an **unknown account** by the router |
| `relay_device` | `account_id`, `device_id`, `first_seen_ms`, `last_seen_ms`; PK `(account_id, device_id)` | the router, on every successful authentication. `first_seen_ms` is written once and `last_seen_ms` is refreshed — the raw material for presence (T-0031), which does not exist yet |
| `machine` | `machine_id` (PK), `account_id`, `name`, `name_key`, `presence`, `last_seen_ms`, `proto_version`, `tombstone_until_ms`, `name_conflict`; `UNIQUE(account_id, name_key)` | the machine directory (T-0043). The router does not touch it, and nothing derives the `presence` column yet |

Everything in this file is **metadata**: an account id, a public key, device
fingerprints and timestamps. No payload ever reaches it — the integration test
routes a marked pane-shaped payload through a real relay and asserts the marker
appears nowhere in the state directory or the logs.

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

A restart is therefore not a data-loss event for anything the relay *knows*: the
durable part is exactly the part that identifies accounts and devices. What it
does drop is in-flight work: an envelope being routed when the process dies is
gone, and the sender learns nothing, because delivery reports live only as long
as the session. Until the durable inbox lands (T-0030), a sender's reconnect
logic is what makes a restart survivable end to end.

## 8. Deployment shapes

### 8.1 Direct QUIC on the host

```console
$ arreo-relay serve --listen 0.0.0.0:8787 --state-dir /srv/arreo-relay
```

The relay binds QUIC on UDP 8787 and logs the non-loopback warning of §4.
Sensible when the host sits on a private network (a VPS with a restricted
security group, a LAN, a VPN interface). The authentication is the device
certificate, not the network — but restrict who can reach the port anyway,
because reachability is the only thing the network gives you.

### 8.2 Loopback plus a UDP forward (the shipped posture)

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

### 8.3 The TCP/WebSocket fallback behind a TLS terminator — not shipped

**This path does not exist in v1.** The routing path is QUIC/UDP only: there is
no TCP listener, no WebSocket upgrade and no HTTP surface in the router. A
network that blocks UDP cannot be worked around by putting a TLS terminator in
front of the relay; today the options are a UDP-capable tunnel (§8.2) or a
different network. The fallback remains a documented direction, not a feature,
and nothing in this binary will answer on a TCP port.

## 9. A worked local example

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

# 3. Run the router.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: router on 127.0.0.1:8787 — loopback only (127.0.0.1:8787)

# 4. Optional: the pairing mailbox beside the router, for devices that do not
#    have a certificate yet.
$ arreo-relay serve --listen 127.0.0.1:8787 --state-dir /srv/arreo-relay \
    --pairing-tcp 127.0.0.1:8770
arreo-relay: state /srv/arreo-relay/relay.db
arreo-relay: pairing mailbox on tcp://127.0.0.1:8770
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

## 10. What to watch in the logs

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
| `<device> disconnected (clean)` / `(error)` | a session ended |
| `session ended: <error>` | a session ended on a protocol or transport error — a malformed frame, an unsupported version, a broken connection |
| `cannot create <dir>: …`, `cannot open <path>: …`, `cannot listen on <addr>: …`, `router stopped: …` | fatal; the process exits 1 |
| `unknown flag …`, `serve needs --state-dir …`, `--listen "…" is not an IP:PORT address: …`, `account add needs …`, `--root-key must be 64 hex characters (an ed25519 public key)` | usage error; exit 2 |

Two silences worth knowing: a rate-limited connection (§6) produces **no log
line at all**, and a `delivered` outcome is not logged — delivery is reported to
the sender on its own stream, not to the operator.

## 11. What v1 does not do, in the operator's terms

These are real gaps, not configuration:

- **`offline` is an answer, not a queue.** A destination with no live session is
  told `offline` and the envelope is dropped. Nothing is stored for later; the
  durable per-device inbox is T-0030. A sender that needs to survive a restart
  must resend.
- **No presence yet.** The relay records `first_seen_ms`/`last_seen_ms` per
  device, but nothing derives online/offline from it, and there is no "last seen
  2 days ago" answer — presence is T-0031.
- **Refusals live only in stderr.** There is no durable audit table to query
  yet (T-0033), so anything you did not capture is gone.
- **No revocation propagation.** The relay verifies the certificate chain and
  the proof of possession, but it has no revocation list (T-0026): revoking a
  device on the server does not stop the relay from admitting its certificate.
  The crude workaround today is re-registering the account with a new root key,
  which invalidates *every* device in it — not a substitute for revocation.
- **The relay does not encrypt.** It carries payload bytes without reading them,
  but it does not make them unreadable. End-to-end confidentiality is the
  daemons' Noise session (T-0023) and wiring the daemon side to the relay is
  T-0050; a client that sends plaintext gives the relay plaintext.
- **QUIC/UDP only** (§8.3), and there is no global connection cap.
