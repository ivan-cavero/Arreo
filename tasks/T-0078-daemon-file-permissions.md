---
id: T-0078
title: The daemon's files are readable and connectable by other local users
status: done
priority: 1
depends_on: []
phase: 2
---

# Goal

Decide and enforce who may read the daemon's state and connect to its socket, then
apply that policy to every file the daemon creates.

## Why this exists

Measured on a running daemon (`umask 0002`, this machine):

```console
the daemon's own socket:   mode=775   <- group-connectable
  <socket>.db              mode=644   <- world-readable
  <socket>.db-wal          mode=644
  <socket>.db-shm          mode=644
  <socket>.lock            mode=664
  <socket>.handoff         mode=775   <- during a handoff; hands out the listener
```

(The identity directory is right — `0700`, `root.key` at `0600` — so T-0025 got its own
mode policy correct. These are the files nobody set a mode for.)

**Proven, not inferred.** A pane printed a token, and the token was then readable
straight out of the store file by anyone who can read the file:

```console
$ arreo spawn secret-pane /bin/sh -c 'echo SUPER-SECRET-TOKEN-12345; sleep 30' --socket $S
$ strings $S.db-wal | grep -c SUPER-SECRET-TOKEN
2
$ stat -c '%a %n' $S.db-wal
644
```

So this is not "a file has an odd mode": **agent output — the operator's prompts, whatever
a tool printed — is readable by every local user**, and the `-wal` sidecar means the most
recent writes are the most exposed.

**The store is the serious one.** `<socket>.db` holds the T-0018 persistence layer —
pane scrollback, which is *agent output*: the operator's prompts, whatever a tool
printed, whatever an agent echoed. It also holds the audit log (redacted, but still
who-connected-when, machine names, pane ids) and the metrics history. On a shared
machine any local user can read all of it with `sqlite3`, and the `-wal` sidecar means
recent writes are exposed too. `644` is what `umask 022` gives a file nobody chmods —
so this is the default on a normal system, not an artifact of this container's unusual
`umask 0002`.

The socket at `775` means a same-group user can drive the daemon: read every pane, send
keystrokes into an agent, spawn and kill panes. The relay makes the *remote* path
authenticated and per-verb gated (T-0023/T-0046); the local path was designed as
"same-machine, therefore trusted", and that assumption is exactly what a mode of `775`
fails to enforce.

## Why priority 1, not 2

Raised from 2 after the security re-review of the server handoff (T-0038 stage 1) named the
consequence that makes this a *security* task rather than a hygiene task: **the socket's mode is
the only gate in front of `Handoff`.** The handoff request is deliberately ungated (the local
socket's trust model is "same machine"), and `connect()` on a Unix socket needs only write
permission on the inode — so at the default `0775`/`0755` a same-**group** peer can request a
handoff, read the nonce the daemon hands back, and drive the cut. The `.handoff` transfer socket
is now `0600` and the descriptors are bound to the requester by a nonce, which is what the
handoff's own review fixed; none of that helps if the *request* can be made by a peer who should
not have it.

So the ordering matters: until this task lands, T-0038 stage 1's security claim is "closed for
same-user, open for same-group at the default mode". That is written here rather than left
implicit, and the code comment in the handoff now says the same thing.

## Scope fence

`crates/arreo-server/src/daemon.rs`, `crates/arreo-server/src/persist.rs`,
`crates/arreo-server/src/handoff.rs`, `crates/arreo-core/src/store.rs`,
`crates/arreo-core/src/lock.rs` (added: the `<socket>.lock` file is created there, and the
policy must apply at creation — the single door, not a chmod at each of the two callers),
plus `SECURITY.md` (the existing policy doc — no new file) stating the policy. The device
authority's files are already correct (`identity/keys.rs`: 0600/0700, measured) and its code
is not in this fence.

## Acceptance criteria

- [x] A **stated policy**, not a scatter of chmods: which of these files may be read by
      the owner only, and which may be reachable by a group (with the reason — e.g. an
      operator who deliberately shares a runtime directory).
- [x] Every file the daemon creates gets the policy at **creation**, not by a later
      chmod: `<socket>`, `<socket>.db`, its `-wal`/`-shm` sidecars, `<socket>.lock`, and
      `<socket>.handoff`.
- [x] An **existing** install is fixed too (a store created before this change is
      re-chmodded when opened, or the policy is documented as apply-on-create with the
      upgrade path named).
- [x] The WAL/SHM sidecars are covered: SQLite creates them, so whatever the answer is
      (chmod after open, `PRAGMA` if one exists, or a containing directory at `0700`),
      it is *tested* — asserting the mode of the main DB while `-wal` stays `644` would
      be a half-fix that looks complete.
- [x] A test asserts the modes after a real daemon start, and fails if any of them is
      world-readable.
- [x] The runtime directory is part of the answer: with `XDG_RUNTIME_DIR` unset the
      default socket lands in `/tmp` (world-writable), so the file mode is the only
      control there — and that is worth saying out loud in the policy.

## Verification

```console
# start a daemon on a scratch socket, then:
stat -c '%a %n' <socket> <socket>.db <socket>.db-wal <socket>.lock
# and prove the leak is real before fixing it:
sqlite3 <socket>.db 'select * from panes limit 1'
```

## Findings

- **The mode of a file nobody chmods is the umask's**, which is why these are `644`/`775`
  on a default system. A security property that depends on the operator's umask is not a
  property.
- **The asymmetry is the tell**: the identity directory is `0700`/`0600` because T-0025
  asked "who may read this?" and the store is `644` because nobody did. Same daemon, same
  directory, two answers — the one that was *decided* is correct and the one that was
  inherited is not.
- **Found while measuring something else** (whether the new `.handoff` socket needed a
  mode), which is how most of these turn up: the question "what mode does this get?" asked
  of one file answered it for five.
