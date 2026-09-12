# ADR 0021 — The server live handoff: adopt the fds, and never be half-dead

Status: accepted (2026-09-12). Task: T-0038. Stage 0 (the adopt primitive) implemented;
stages 1–4 follow.

## Context

§3.13: *"updates must never kill agents, drop sessions, or force anyone to save their work."*
The client half is easy (ADR 0020) because the daemon owns the PTYs. The server half is the
hard one: replacing the daemon means replacing the process that **holds the PTY masters**,
and a PTY master is not a file that can be re-opened — it is the kernel's handle on a
pseudoterminal whose slave is the controlling terminal of a running agent. Kill the daemon
naively and every agent on the machine dies with it.

So the new daemon must **inherit the master descriptors** from the old one, and the old one
must survive long enough to be the fallback if the new one cannot take them.

## Decision

**1. Adopt the master fds over `SCM_RIGHTS` — never restart a pane.**

A PTY master is passed as a file descriptor over a Unix socket. On the receiving side the
daemon builds a `Pane` around it: the same reader pump, the same ring buffer, the same public
API. The agent inside never learns anything happened. `portable_pty::MasterPty` is a public
trait, so the adopted master implements it directly rather than re-opening the device.

**The child is not our child.** The new daemon did not fork the agent, so `waitpid` cannot
report it (it is reparented to init when the old daemon exits). Exit is therefore detected
from the master fd — end-of-stream or `EIO` means the slave side is gone — with `pidfd_open`
(Linux ≥ 5.3) giving a definitive status where it exists. A pane that dies during the cut
reports `exited (code unknown)`, because that is true, rather than `0`, which is not.

**2. Two phases, with the old daemon serving until the new one commits.**

```text
new daemon                              old daemon
────────────────────────────────────────────────────────────────
start
  connect to the admin socket
  version handshake          ────────▶  check the N−1 window
  "ready to receive"         ◀────────  send fds + manifest
  adopt every pane
  open SQLite (old closed it)
  "committed"                ────────▶  old daemon exits 0
  serve                                  (or: aborts, and never exits)
```

Every failure before *committed* ends the **new** process: the old daemon keeps serving and
its fds stay valid, because a transferred descriptor is a *duplicate* — sending it does not
close ours. There is no state in which the machine has no serving daemon.

**2b. The fd channel is dedicated, and the pid it carries is verified.**

Two properties of the transfer, both found by the security review of stage 0 and both
easy to get wrong in stage 1:

- **Ancillary data is a barrier on a stream socket** (unix(7)): the kernel delivers a
  descriptor together with whatever bytes were already queued ahead of it. So a channel
  that carries protocol bytes *and* descriptors makes the transfer ambiguous — a stray
  byte is either mistaken for the transfer's marker or burns a legitimate transfer. The
  descriptors therefore travel on their **own socketpair**, drained to the transfer
  boundary, and the byte that accompanies a descriptor is a synchronisation marker, not
  an integrity control: the integrity property is the kernel's `SCM_RIGHTS` delivery
  itself. A channel that must also carry protocol bytes has to frame around the transfer.
  (`SOCK_SEQPACKET` would give real message boundaries — the same socketpair carries the
  version handshake — but **macOS has no `AF_UNIX` `SOCK_SEQPACKET`**, so requiring it
  would make the macOS handoff impossible. A stream pair plus this discipline it is.)

- **The claimed child pid is verified against the terminal before it is used.** The pid
  arrives over the same channel as the descriptor, so on its own it is a number an
  attacker chooses — and a pid is not inert: `pidfd_open` on a same-user process needs no
  permission check, so a wrong pid makes `kill` signal an unrelated process and `try_wait`
  attribute its death to the pane. The master's session leader is exactly the pid the
  spawning daemon created (`setsid` + `TIOCSCTTY`), so the pid is accepted only when
  `TIOCGSID` on the adopted master agrees, and a mismatch refuses the adoption with both
  numbers named. Refusing is safe: the old daemon keeps serving (see §2), so a bad pid
  costs a retry, not an agent.

  **And the third case, which is not a refusal:** a terminal that names *no* session cannot
  confirm the claim, so the pid is **dropped** — never watched, never signalled, never
  attributed (`child_pid()` is `None`, `kill` reports unsupported) — and **nothing is
  concluded about the child**. The pane is watched by end-of-stream on the master, the same
  rule the no-pid path uses.

  **That last part is the subtle one, and getting it wrong is how this design first went
  wrong.** The tempting reading is "no session ⇒ the session leader exited ⇒ the pane
  arrived dead", and it is false: `ENOTTY` means only that the *terminal* has no session,
  which is possible **while the child is alive**. Measured on this kernel — a child that
  calls `setsid()` and never takes the controlling terminal, still writing to the slave —
  the master answers `ENOTTY` and the child is in state `S` throughout. Reporting that pane
  dead would have been a false negative with teeth: the daemon advertises a live agent as
  exited, tears down its attach stream, and the kill switch silently no-ops because a
  dropped pid means `kill` reports unsupported. So the ENOTTY branch drops the pid and
  concludes nothing, which is correct for the live case and still converges for the dead
  one.

  The invariant is what matters — *no unverified pid ever reaches a signal or an
  attribution* — and it holds in all three branches; only the mismatch is fatal to the
  transfer.

  This holds on Linux, where `TIOCGSID` on a session-less master returns `ENOTTY`
  (measured). On Darwin it is [INFERENCE] from XNU's `ptyioctl`/`ttcompat`: if `TIOCGSID`
  there behaves differently, the consequence is the *safe* branch above — the pid is
  dropped and the pane is watched by end-of-stream — not a wrong pid being trusted. That is
  a macOS CI-runner check, not something a Linux build can settle; it is recorded in T-0038
  as a cross-OS gate.

  **The refusal is side-effect free.** Nothing about the inherited terminal is changed until
  every check that can refuse has passed: the geometry repair happens *after* the pid is
  settled and after the child's `dup`, not inside the master's constructor. A refusal that
  had already resized the sender's live terminal (and SIGWINCHed its agent) would be a
  mutation the old daemon keeps serving without knowing about, so the order is part of the
  contract rather than an implementation detail.

  **And a bare pid is re-checked before it is signalled.** Where no pidfd exists (any
  non-Linux platform, Linux < 5.3, or a `pidfd_open` refusal such as `EMFILE`), the number is
  the only handle on the process, so the killer keeps a descriptor on the terminal and
  re-reads `TIOCGSID` immediately before signalling — a pid confirmed once at adoption can
  name an unrelated process by the time a kill arrives, and a child that has exited needs no
  signal, so refusing there loses nothing.

**3. One serving daemon, enforced by `flock`.**

The daemon holds an exclusive lock on its socket path for its whole life. A second daemon
cannot start while one holds it, and the kernel releases it when the holder ends — cleanly or
by `SIGKILL`. This is the same reasoning as ADR 0020's update lock: a stale lock cannot arise,
so there is no pid-liveness probe and no timeout. It also makes the handoff's "exactly one
serving daemon" an invariant rather than a convention.

**4. Scrollback and state are pointers, not payloads.**

Scrollback is already disk-backed (T-0018) and the durable state already lives in SQLite
(store v7), so the handoff transfers a *manifest* — pane ids, their specs, their sizes, their
child pids — and the new daemon re-opens what is already on disk. The alternative (serialising
scrollback through the socket) would make the cut's cost proportional to history length, which
is exactly the wrong shape for the one operation that must be fast.

SQLite: the old daemon checkpoints the WAL and closes before the new one opens, so there is no
window with two writers. An audit write during the cut loses nothing, and a corrupt `-wal`
heals by T-0018's existing rule.

**5. A protocol break becomes a deferred update, never a half-handoff.**

The handshake refuses a new daemon whose protocol version is outside the N−1 window
(ADR 0017). The machine keeps serving the old binary and the UI says "update pending". An
update that cannot complete is a *scheduled* update, not a failed one.

## Why not the alternatives

- **Restart the daemon via the service manager.** This is what every other tool does, and it
  is precisely what §3.13 forbids: systemd stops the old process, the PTY masters close, and
  every agent gets `SIGHUP`. The requirement exists because this is the normal behaviour.
- **Fork the new binary and exit at once.** No rollback: if the child fails while adopting the
  fifth of eight panes, the old daemon is already gone and five agents are orphaned with no
  daemon. The two-phase commit exists to make that state unreachable.
- **Re-open the PTY devices by path.** A new process can open `/dev/pts/N`, but it is not the
  session leader and holds no relationship to the child; the resize signal, the job control
  and the exit relationship are all attached to the *master fd*, so the fd is the thing that
  has to move. (This is also why the adoption is `SCM_RIGHTS` and not a path.)
- **Serialise the agents and re-spawn them.** Restarting an agent destroys its context,
  which is the exact harm being prevented. Non-starter, recorded because "just restart it" is
  the tempting simplification.
- **A proxy that owns the fds, with daemons behind it.** A permanent extra process, an extra
  hop on every read, and a new single point of failure, to avoid a transfer that happens a few
  times a month. Rejected on simplicity (§5.9): the handoff is rare and the proxy is always.

## Consequences

- **The interesting failure is abort-safety, and it is testable**: kill the new daemon at each
  of the three points (before the transfer, mid-transfer, after the ack but before commit) and
  assert every pane is alive, the old daemon is still serving, and a retry succeeds.
- **Windows is a different story** (T-0039): ConPTY pseudoconsole handles can be inherited, but
  the fallback — swap the binary and take effect at the next restart — is what ships if that
  proves unreliable on some OS build. "Never forced while agents run" holds either way.
- **The relay session token travels in the same payload** (§3.13 step 3), so a machine that
  hands off does not re-handshake with the relay — the field is reserved now and filled when
  the relay crates land.
- **Stage 0 is the primitive everything else rests on**: `Pane::adopt` plus the fd-passing
  helpers, tested by passing a real master over a socketpair and reading back through the
  adopted pane. Stages 1–4 build the protocol, the manifest and the abort paths on top of it.
