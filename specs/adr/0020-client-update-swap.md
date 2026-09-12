# ADR 0020 — The client update: one atomic rename, an OS lock, and a hand-over that carries its path

Status: accepted (2026-09-12). Task: T-0070. Supersedes nothing.

## Context

§3.13 promises that updates install **without killing agents or dropping sessions**. The
daemon owns the PTYs, so the client update is the easy half — but it is also the half that
can *brick an install*: a client that replaces its own binary badly leaves a file the user
cannot run to repair it, and no daemon-side cleverness recovers that.

Three questions had to be answered, and each had an obvious answer that is wrong:

1. **How is the binary replaced?** The conventional sequence is rename the current binary
   away, then rename the new one in.
2. **How is "one update at a time" enforced?** The conventional answer is a pid file.
3. **What does the process do after installing?** The conventional answer is to re-exec
   itself and rebuild the path with `current_exe()`.

## Decision

**1. Link, then one rename — not two renames.**

```text
copy the candidate to <binary>.staged      (same directory ⇒ same filesystem)
hard_link(<binary>, <binary>.prev)         (a second name for the old inode)
rename(<binary>.staged, <binary>)          (atomic replace — one syscall)
```

`rename(2)` over an existing path is atomic, so there is **no instant where the binary path
is missing or non-executable**. The hard link gives `.prev` a second name for the old inode
without moving it, so a crash between any two steps leaves a runnable binary at the path
(old before the rename, new after it) *and* a rollback target.

**2. An OS file lock — not a pid file.**

`File::try_lock` (std, since 1.89) holds the lock in the open file description. The kernel
releases it when the process ends — cleanly, by signal, or by `SIGKILL`. So "update already
in progress" cannot be wrong in either direction, and there is no stale-lock case to handle.

**3. The hand-over carries the path it already resolved.**

`install()` resolves the current binary **before** the swap and passes that path to the
re-exec, because after the swap this process's own path is stale: the swap renamed a new
file over it, so `/proc/self/exe` names the *unlinked* old dentry and is reported as
`… (deleted)`. Anything that re-derives the path after the swap fails.

## Why not the alternatives

- **Two renames (the conventional sequence).** A crash inside the window leaves no binary at
  the path at all: the install cannot be run to repair itself, and the operator's only way
  back is a reinstall. This was not theoretical — the task that specified this work *asked
  for crash-injection between the two renames*, which is a test of a window that the hard
  link removes. **Cost of the choice:** `.prev` must be on the same filesystem as the binary
  (a hard link cannot cross one), which is why both are siblings; and a package-manager
  install whose directory this user cannot write fails at the link with a clear error rather
  than half-succeeding.
- **Copy-then-overwrite.** `write(2)` truncates first: a crash mid-write leaves a *partial*
  binary at the path, which is strictly worse than a missing one (it exists, so tools trust
  it; it is truncated, so it cannot run).
- **A pid file.** Answers "is another update running?" by guessing: a pid that is alive may
  be an unrelated process that reused the number, and a pid that is gone may be a live
  updater in a different pid namespace. Every implementation ends up with a timeout — a
  guess about time rather than a fact about a process. The OS lock makes the failure mode
  unreachable.
- **Re-deriving the path after the swap.** It *looks* more robust (ask the system where I
  am) and is exactly backwards: the system can only answer about the image that was loaded,
  and the swap deliberately replaced that. The path is known before the swap; the honest
  move is to keep it.
- **`--from <path>` instead of fetching a release.** The anonymous path needs the signing key
  of T-0036, whose custodian has not created it (the point of an offline key is that an agent
  does not hold it). Rather than fake a channel, the verb takes a binary the operator already
  has — a real feature (installing a build you made) and the place a channel will feed in.

## Consequences

- **The invariant is checkable**: the whole module touches a binary path, a `.prev` sibling and
  the resume token, and contains no `kill`, no `waitpid` and no service-manager call. The
  `update` slice proves it by holding a real daemon and eight panes across a real update.
- **`--rollback` is always possible** after a successful install, because `.prev` is a name for
  the very inode that was in place.
- **Windows needs its own shape** (T-0039): a running image cannot be replaced, only renamed
  away, so its swap keeps a bounded window and restores the old binary if the second step
  fails. The control flow is one; the syscalls differ.
- **Two failure modes are surfaced, not swallowed**: a candidate that cannot run is refused
  before the swap, and a path this user cannot write names the package manager that owns it.
