# `arreo machines` — the account's machine directory

The machine directory (ROADMAP §3.7) lives at the relay: it holds names, ids and
presence, and nothing else. `arreo machines` is how a human reads it and how a
script consumes it.

```console
arreo machines list   [--json] [--all] [--offline] [--config PATH]
arreo machines status [<name>] [--json] [--offline] [--config PATH]
arreo machines rename <old> <new> [--config PATH]
arreo machines remove <name> [--stale] [--force] [--config PATH]
arreo machines add    <pairing-code> --uri <invite> [--name N]
arreo machines trust  <device> [--machine <name>] [--role viewer|operator] [--yes]
arreo machines trust  --list [--json]
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
  rule gave this machine a suffix — not a mistake), `name-tombstoned` (the
  machine was removed and its name is held for the tombstone window),
  `name-reclaimable` (the machine is stale, so its name can be taken),
  `unverified` (the row came from the cache, see below).
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

`--config PATH`, else `$ARREO_CONFIG` — and deliberately **no default path**,
for the same reason the daemon has none: a CLI that invented its own
`$XDG_CONFIG_HOME/arreo/arreo.toml` would be a second answer to "which file is
this machine's relay configuration". The file is the same one the daemon reads,
parsed by the same code (`arreo_core::relay::config`) — the dependency rule
forbids the CLI depending on `arreo-server`, and a second parser would be a
second answer to "what is a valid `[relay]` section".

No configuration at all is exit 2 (the operator has to name the file). A
configuration that names no relay — an absent file at the path given, no
`[relay]` section, or `enabled = false` — is exit 4: the directory is
unreachable and the machine knows why.

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

## Writing: rename and remove

Both write through the relay, which applies the directory's rules — the client is
never trusted to decide whether a name is free or a machine is stale.

`rename <old> <new>` renames a machine. A name that is live for another machine is
**refused with nothing changed** (exit 5), and the result printed is the row the
directory now holds. Renames are not suffixed: the deterministic suffix is the rule
for *claims*, where a machine joins and asks for a name it may not get; an operator
who renames a machine means that name.

`remove <name>` tombstones the name for 30 days: the row stays (so the machine
keeps its name if it comes back), and no *other* key can take the name until the
tombstone expires. The output says until when. Two things make it deliberate:

- a machine that is **online right now** needs `--force`, because tombstoning a
  machine that is answering is almost always a mistake;
- `--stale` is the bulk form: it removes every machine the presence rule calls
  stale, prints each reclaimed name, and is idempotent (a second run finds
  nothing).

`--json` and `--offline` are for the read verbs and are **refused** here rather
than accepted and ignored: the write verbs print one line, which is not a
contract, and a write cannot answer from memory.

## The first machine

Everything below this section is about adding a machine to an account that
already has one. This is how the first one gets there — and it is the step every
account starts with, so it comes first.

An account's root key *is* its identity: the relay accepts a device certificate
only if it verifies under the key the account was registered with. So the order
is: this machine makes the key, the relay is told the key, and then this machine
admits **itself** as the account's first device.

```console
# 1. This machine makes its root key, and prints the public half.
#    (Any verb that loads the identity does this; the fingerprint is what you want.)
$ arreo devices list
root 17d0a47fbfc70f63d1e9a44e2b1c933d51f6b0c8e4a927d3f1c05b8a6e2d4f709
no devices paired yet (pair one, or issue from a public key)

# 2. Register the account at your relay with that key.
$ arreo-relay account add --state-dir /var/lib/arreo-relay \
    --account my-account --root-key 17d0a47fbfc70f63d1e9a44e2b1c933d51f6b0c8e4a927d3f1c05b8a6e2d4f709

# 3. This machine admits itself. Two steps, because the first one waits for a
#    joiner — and here the joiner is step two, in another shell or the same one.
$ arreo pair                                  # prints four words, then waits
$ arreo pair --join "four word phrase" --uri 'arreo://pair?v=1&…' --name workbox

# 4. Now the daemon can join the relay and assert this machine's directory row.
$ arreo-server --socket ~/.local/share/arreo/arreo.sock --config /etc/arreo/arreo.toml
arreo-server: directory: this machine is workbox
```

**Where the key lives, and which half you want.** The machine's identity directory
holds two files that are easy to confuse:

| File | Holds | Use |
| --- | --- | --- |
| `identity/root.key` | the **secret** seed (64 hex characters) | never leaves the machine |
| `arreo devices list` → `root …` | the **public** key (64 hex characters) | this is what `account add --root-key` takes |

Registering the secret by mistake is accepted by the relay and then fails much
later, when a certificate is rejected for not verifying — a confusing place to
learn about a mixup. `arreo devices list --json` carries the same public value as
`.root` for scripts.

A self-admitted machine is granted the **viewer** role by the pairing default.
That is sufficient to observe it; grant more from the account's machines with
`arreo machines trust` (see [Trust](#trust-who-may-use-this-machine) below) if you
want this machine to drive other machines, or be driven. If the machine is the
account's owner, it already holds the root, so it can issue itself any role:

```console
$ arreo pair --role owner            # on step 3, instead of the default
```

## Joining: `add`

```console
# on the machine that already belongs (it holds the account root):
arreo pair --config /etc/arreo/arreo.toml       # prints a code and an invite

# on the machine being admitted (no configuration needed):
arreo machines add "four word phrase" --uri 'arreo://pair?v=1&…' --name the-pi
```

`add` runs on the machine being **admitted**, and it is the last piece of
"machines are first-class citizens": one code, one invite, and the machine is in
the account. The invite carries the account and the relay because the joining
machine has no configuration to read — it is joining *because* it has none, and
the machine that admits it is the only party that knows both. The reasoning, and
the alternatives rejected, are in
[ADR 0018](../specs/adr/0018-machine-join-handoff.md).

What happens, in order:

1. The SPAKE2 pairing exchange — the same one `arreo pair --join` runs — issues
   this machine a **certificate** signed by the account root. Only a machine that
   holds that root can issue one the relay will accept, which is why the
   admitting side must be a machine that already belongs.
2. The certificate and its device key are saved, and the admitting machine's key
   is pinned. Nothing is written until the certificate verifies.
3. The machine connects to the relay and asserts **its own** directory row, under
   **its own** root key. Being admitted and being registered are two different
   things, and only the second makes it visible to the account.

It prints the device it is now known as, the granted name, and its machine id.
If the name was live for another machine, T-0043's rule gives the deterministic
suffix and `add` says so — the granted name is what is true.

Refusals, and what they mean:

| Exit | Situation |
| --- | --- |
| 2 | no code, no `--uri`, a malformed invite, or a flag that belongs to another verb |
| 4 | the invite names no account and relay (the admitting machine had no `[relay]` configuration), or the relay named in it cannot be reached |
| 5 | the relay would not register the machine (a name the rule rejects) |
| 1 | the pairing exchange itself failed — a wrong or expired code, or an unreachable mailbox |

A machine that is admitted but cannot reach the relay keeps its certificate and
is told so; run `arreo machines add` again with a fresh code once the relay is
reachable. Pairing codes are single-use (T-0024), so re-joining always takes a new
one.

## Trust: who may use *this* machine

An account's certificate says what a device *is*. It does not say what a device
may do **here** — that is this machine's own decision, stored in its own database,
and it is the reason a phone paired to the VPS is not automatically trusted by the
Pi (ROADMAP §3.7). The model and the rejected alternatives are in
[ADR 0019](../specs/adr/0019-per-machine-device-trust.md).

```console
# on the machine whose access you are changing:
arreo machines trust --list
arreo machines trust dev_4f55... --role operator --yes
arreo devices revoke dev_4f55... --machine workbox
```

What a refused device is told — the command it should be handed:

```
machine workbox has no grant for this device, so Read is refused.
Grant it with: arreo machines trust dev_4f55... --machine workbox --role viewer --yes
```

**Two facts, two commands, never conflated:**

| Command | What it changes | Where it applies |
| --- | --- | --- |
| `devices revoke <id>` | the **device** — its certificate no longer authenticates | every machine (account-level) |
| `devices revoke <id> --machine <name>` | **this machine's grant** | one machine; the device keeps its access elsewhere |
| `machines trust <id> --role ...` | **this machine's grant**, extending or restoring it | one machine |

Trust is **local and cannot be delegated**: `--machine` must name this machine, or
the command is refused (exit 5). No machine — and not the relay — can grant on
another's behalf, because a grant recorded anywhere but the machine that will
enforce it would be advice, not access.

`machines trust` also refuses a device this machine has never **pinned** (exit 3):
the Noise handshake resolves a peer from the pin list, so a grant for an unpinned
key could never be used — refusing catches a mistyped fingerprint instead of
recording it. Without `--yes` the device fingerprint and the role are shown and a
confirmation is required; an authorization that writes itself when a human hits
enter is how the wrong device gets trusted.

Grants are `viewer` (observe) or `operator` (also drive); v1 has no finer grain.
The default is `viewer`: widening a grant is easy, noticing one you did not mean is
not. `owner` is accepted as a synonym when reading certificates, but this surface
prints and documents the roadmap's word.

### The same surface in the TUI

`arreo-tui` keeps the two panels behind `m` (machines) and `g` (trust), with the
CLI's own refusals and the same confirmations: removing a machine behind a
confirmation that names it (an online machine's confirm adds the `f` force key —
plain `y` is the plain remove, so the relay's refusal still has to happen for the
force to be asked), granting with the fingerprint shown and confirmed before
anything is written, revoking confirmed by the device id. A TUI pointed at
another machine (`--machine <name>`) manages that machine's panes but **cannot**
list, grant, or revoke its trust: the panel shows the CLI's own sentence —
`machines trust: <device> is not this machine (which is <name>, <this machine's id>). Trust is local: no machine — and not the relay — can grant on another's behalf. Run this on <device> itself.` — exit 5 semantics —
because a grant written anywhere but the enforcing machine would be advice. The
reason is the same one this page states for the CLI: trust does not delegate.

### The trail

Every grant, every cut, and the first refusal of each session appends an audit row:

```
trust.grant   kind=trust outcome=ok      device=dev_4f55... agent=workbox detail=machine=2cfb51f3... role=operator by=4f55...
trust.revoke  kind=trust outcome=ok      device=dev_4f55... agent=workbox detail=machine=2cfb51f3...
trust.refuse  kind=trust outcome=refused device=dev_4f55... agent=workbox detail=machine=2cfb51f3... verb=Read reason=...
```

`device` is the device the decision is **about**; the actor rides in `detail`
(`by=...`), and the machine is named both ways — by name in `agent` for a reader, by
id in `detail` so an exported row survives a rename. The refusal row is written
**once per session**, not once per refused verb: a client that retries cannot fill
the operator's log from outside.

Upgrading a machine to this feature grants every device it had already pinned the
default role **once**, and the row says `reason=backfill`. Without that, an upgrade
would lock out every existing pairing — and without the one-way marker, an
operator's deliberate "revoke everything" would quietly heal itself on restart.
