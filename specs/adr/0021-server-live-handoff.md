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

  **And on stage 1 it became a dedicated connection, not a pair on the client socket.** The
  client socket cannot be made safe for descriptors by reading carefully: the kernel hands a
  descriptor to whichever `recvmsg` reads the byte it rides on, so an ordinary buffered
  asynchronous reader that happens to read that byte makes the kernel **discard the
  descriptor** — silently. A lost listener descriptor means the outgoing daemon exits, the
  new one has no socket, and the machine is left with no daemon at all: precisely the
  half-dead state §2 exists to prevent, arrived at by a means the FD-transfer tests would
  never see (they pass on a `socketpair` where nothing else is written). So the descriptors
  travel on their own connection to `<socket>.handoff`, which is bound **only while a handoff
  is negotiated** and unlinked when it ends, and on which nothing but marker bytes and
  descriptors is ever written. The topology makes the hazard unreachable rather than
  disciplined — the same move as the hard link in ADR 0020, which removed a window rather
  than testing inside it.

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

**2c. The transfer is authenticated, and the commit is positive evidence.**

Both of these came out of the mandatory security review of stage 1, and both were
*reproduced* against the running binaries rather than reasoned about — which is the only reason
they are in this ADR rather than in a release.

**The commit must be evidence, not absence.** The first implementation treated end-of-stream on
the transfer connection as "the incoming daemon is serving". It is not: EOF says only that a
peer stopped writing. Measured — a local process sent `Handoff`, read `HandoffReady`, connected,
called `shutdown(SHUT_WR)` and did nothing else, and the outgoing daemon **exited 0 within
40 ms** with nobody serving: a client's connect hung on the stale backlog and the audit log
recorded a cut that never happened. That is the half-dead state §2 exists to make unreachable,
reached by the cheapest possible input. The incoming daemon now sends an explicit **commit
marker byte** once its accept loop owns the inherited listener; EOF before it is an abort.

The generalisation is worth more than the fix: **a protocol may not infer success from a
peer's silence, and a check that a peer is gone is not a check that its successor is
running.** Everywhere else in this design, "the other side stopped" and "the other side
succeeded" are different facts, and they are tested as different facts.

**What the marker does and does not prove.** The security re-review pushed back on this
section's first wording, correctly: the byte is **authorisation, not evidence that the peer is
serving**. The protocol cannot see the far side's accept loop, and a peer that presents the
nonce, takes the descriptors, sends the byte and then does nothing will leave the outgoing
daemon exiting 0 — reproduced. What the byte does establish is *who* sent it: only the process
that asked for the handoff on the main socket could ever read the nonce, so the marker comes
from the requester and not from a bystander. The shipped binary sends it after its accept loop
is live; a hostile peer is free to lie about that, and the defence against a hostile peer is
§2c's authentication and the local socket's trust model, not this byte.

A stronger check was considered and **rejected**: after receiving the marker, connect to
`<socket>` and require a `Welcome` before exiting. It sounds like exactly the empirical rigour
this project asks for, and it introduces a worse fault — a probe that times out against a
healthy-but-slow incoming daemon would leave the outgoing daemon serving *and* the incoming
daemon committed, which is the two-daemons-on-one-socket failure (§2c) reintroduced by the
check meant to prevent a different one. One byte is one atomic decision; a probe is two.

**Descriptors require authentication, in three layers that defend different things.** A PTY
master is worth stealing, and the transfer socket was as permissive as the umask allowed
(`0775` measured; `connect()` on a Unix socket needs only write permission on the inode, so any
same-group user qualified, and under `umask 0` anyone). Whoever won the race received the
listening socket and the lock, the outgoing daemon exited 0, and the attacker served the socket
as the daemon — a full local impersonation of the machine's agent runtime.

- **Mode `0600` on the transfer socket**, at bind time. Cheap, and it is the only control that
  does not depend on the directory: with `XDG_RUNTIME_DIR` unset the daemon's socket lives in
  `/tmp`, mode `1777`, so the *location* is not a protection.
- **`SO_PEERCRED`**: the peer's uid must be ours.
- **A per-handoff nonce**, minted by the outgoing daemon, returned in `HandoffReady` **on the
  main socket**, and required as the first bytes on the transfer connection. This is the layer
  that binds the transfer to *the process that asked for the handoff* rather than to any process
  that noticed the path — and it is why the request and the descriptors travel on two different
  channels rather than one.

What this does **not** claim: a same-user process can still request its own handoff, because the
local socket's trust model is "same user, same machine" and this feature does not change it. The
nonce binds the *transfer* to the requester; it does not make the requester trustworthy. That is
stated in the code as well, so nobody re-derives the stronger claim from the mechanism.

**And the request side is only as narrow as the main socket's mode.** The handoff request is
ungated by design, and `connect()` on a Unix socket needs only write permission on the inode —
so at the default `0775`/`0755` the trust model this section relies on is "same **group**", not
"same user": a group peer can ask for a handoff, read the nonce the daemon returns, and drive
the cut, which the re-review measured as reachable (it could not become a second uid to complete
the connection, so the connect step is [INFERENCE] from the permission rule). The three layers
above harden the *transfer*; none of them narrows who may *ask*. Narrowing the socket's mode is
T-0078's decision and it is now priority 1 with this consequence named, because until it lands
this design's honest claim is "closed for same-user, open for same-group at the default mode".

**And one handoff at a time — on its own lock.** Two concurrent handoffs both committed and left
**two daemons serving one socket** (reproduced five times out of five), because `flock` lives in
the *open file description*: two processes that inherit the same description both "hold" it, so
the inherited lock is structurally incapable of enforcing exclusivity among handoff
participants. Exclusivity therefore needs its own lock on the transfer path, held by the outgoing
daemon for the duration of a handoff. **An invariant enforced by an inherited descriptor is not
enforced by it** — the same confusion that made the received lock descriptor need validation
against the path rather than a probe of the path.

**2d. The panes travel by descriptor and by memory, and the pause is the atomic point.**

Stage 1 moved the socket and the lock; stage 2 moves the agents. Two different things have to
cross, and they need different mechanisms:

- **The master descriptor** — the kernel's handle on the terminal (stage 0's `Pane::adopt`).
- **Everything the old daemon had already read into memory** — the scrollback a client sees and
  the journal the state engine is derived from. A read consumes: bytes the outgoing daemon has
  read are *gone* from the pipe, so if only the descriptor crossed, the incoming daemon would
  start with an empty scrollback and every client would watch the history vanish at the cut.

**The transfer point must be atomic with respect to `read` → `push`.** The pump reads from the
master and pushes into the ring, and the bytes between those two steps are in flight — not yet in
the ring, no longer in the kernel. A snapshot taken then loses them silently, which is the worst
shape of bug this project has: output that disappears with nothing to point at. The buffer lock
cannot be the quiescence point, because `read` blocks indefinitely on an idle pane and the
snapshot would wait forever behind it. So each pump checks a pause flag **before** it reads, and
acknowledges when it has finished the read+push it was in; once every pump has acknowledged,
nothing is in flight and the snapshot is exact.

**And every abort must resume the pumps — this is the requirement the design nearly missed.**
A paused pane whose child keeps writing fills the kernel pipe buffer (commonly 64 KiB) and then
**the child blocks**. If a handoff aborts while the pumps are paused and nobody resumes them,
that agent is stuck until the daemon restarts — a worse outcome than the update simply not
happening, arrived at by the feature meant to protect it. So the outgoing daemon resumes before
it returns to serving, on every abort path, and a test proves it by pushing well over a pipe
buffer through a paused pane and then aborting.

**The incoming daemon does not read until it has committed.** If it started its pumps early and
then aborted, the bytes it had consumed would be gone and the outgoing daemon — resuming with a
hole in its scrollback — could not get them back. So it adopts, seeds, starts the accept loop,
commits, and only *then* begins reading. Bytes written in the meantime wait in the pipe buffer,
which is exactly what makes the ordering safe.

**What travels is state; what is derived is re-derived.** The scrollback (lines plus an
unterminated partial), the raw journal, and the pane's identity travel. The state engine's state
and its feed cursor do **not**: the engine is a pure function of the journal, so the incoming
daemon feeds it the transferred journal once and gets the same answer, including a pane that was
mid-question at the cut. Transferring the engine's state instead would create a second source of
truth for the same fact — the defect class this codebase keeps finding — and would leave the two
free to disagree. The metrics sampler is derived the same way (it samples `/proc`, which is the
one place process facts live).

**Enforcement travels as a path, not a descriptor.** A cgroup is a *named* kernel object
(`/sys/fs/cgroup/...`), so the guard is re-opened from its path on the other side rather than
passed. A pane that arrived without its guard would silently lose its memory ceiling, so the
incoming daemon either re-opens it or says so — serving an agent unprotected while reporting a
clean handoff is the failure that would matter most and show least.

**Rejected: reusing the crash path.** T-0018's restore re-spawns the recorded command, which is
right for a reboot (a descriptor cannot survive one) and wrong here: it produces a *new* child,
and the whole point of §3.13 is that the agent keeps running. Two mechanisms for two different
problems, and the reboot path is not a shortcut for this one.

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

SQLite needs **nothing** here, which is a correction to this ADR's first draft (§4 originally
said "the old daemon checkpoints the WAL and closes before the new one opens"). That was
written before reading the store: there is no long-lived handle to close. `SessionStore` is
opened per operation and dropped (`daemon.rs:140,221,495,591,1723`) — one connection per audit
row — and the only long-lived connection in the process is the device authority's, on the *same
file*. WAL and a 5-second busy timeout are already configured (`store.rs:315,321`) for exactly
this reason: the daemon holds a connection while the CLI opens its own. So two processes on the
DB is an existing, supported situation rather than a hazard to engineer around, and the
handoff's overlap window is a second process doing what the CLI already does. The lesson is
narrow but worth keeping: **the ADR described a mechanism nobody had checked for; the store
turned out not to need one, and the risk of believing the ADR was implementing a checkpoint
that the design does not require.**

**5. A protocol break becomes a deferred update, never a half-handoff.**

The handshake refuses a new daemon whose protocol version is outside the N−1 window
(ADR 0017). The machine keeps serving the old binary and the UI says "update pending". An
update that cannot complete is a *scheduled* update, not a failed one.

**The check is the incoming daemon's, and the direction matters.** It is
`negotiate(new_protocol, [old_protocol])` — the incoming daemon is the one that must speak the
outgoing daemon's protocol, so it is the `server` argument of the negotiation. Written the
other way round (`negotiate(old, [new])`) the window is `{old, old-1}`, which **refuses every
forward version bump** — that is, it would refuse exactly the update it exists to perform, and
only ever accept a no-op or a downgrade. An update that bumps the version is the normal case,
so getting this backwards would make the feature useless in a way that looks like a version
policy rather than a bug.

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
- **The relay session is restarted, not carried** — also a correction. §3.13 step 3 says the
  relay session tokens transfer, but nothing to transfer exists: `RelayContext` has no resume
  field, and the relay task's `JoinHandle` is discarded at spawn (`main.rs:150`), so the live
  connection is owned by a `RelaySession` whose `Drop` aborts its pumps. The new daemon
  therefore dials its own session, and the relay sees a re-register rather than a
  continuation. That is safe, not merely expedient: T-0060 already made a second session for
  one device *end the session it replaced* and recover in ~289 ms, which is the same event a
  handoff now produces. Carrying the session is an optimisation with no requirement behind it,
  so it is not planned.
- **Stage 0 is the primitive everything else rests on**: `Pane::adopt` plus the fd-passing
  helpers, tested by passing a real master over a socketpair and reading back through the
  adopted pane. Stages 1–4 build the protocol, the manifest and the abort paths on top of it.
