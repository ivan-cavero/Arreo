# Worktrees — one checkout per pane (T-0091)

**One sentence: `arreo spawn --worktree` gives a pane its own `git worktree` on its
own branch, so two agents on one machine cannot touch each other's files.**

```console
arreo spawn <id> <program> [args...] [--socket PATH] [--worktree [NAME]]
arreo worktrees list   [--socket PATH] [--repo PATH] [--config PATH] [--json]
arreo worktrees remove <pane> [--force] [--repo PATH] [--config PATH] [--socket PATH]
```

## The collision this prevents

Every pane without this flag shares one working directory: the daemon's. Two agents
launched there — a reviewer and a fixer, a planner and a worker — edit the same
files in the same checkout, and neither can see the other's edits until something
breaks. `git stash` and hand-made copies are the workarounds people reach for, and
both lose work: a stash is invisible to the agent that did not make it, and a copy
does not share the object store or the refs.

A **worktree** fixes the shape of the problem rather than the symptom:

- each pane gets its own working directory, so two agents' file edits are disjoint
  by construction (the `worktree` e2e slice writes the same filename in each pane's
  worktree and reads both back);
- the branch is real — it can be merged, pushed and reviewed, unlike a stash;
- the object store and the refs are shared, so a second checkout costs a directory
  rather than a second clone.

## Spawning a pane in its own worktree

```console
$ arreo spawn worker-1 omp --worktree
spawned worker-1 (worktree worker-1)

$ arreo spawn reviewer /bin/bash --worktree review-pass-2
spawned reviewer (worktree review-pass-2)
```

- `--worktree` **with no value** uses the pane id as the name: `worker-1` above.
- `--worktree <name>` uses `<name>`. A following argument that starts with `-` is
  left alone (it belongs to the program), so `--worktree` at the end of the line
  always means "use the pane id".
- The pane's **cwd is the worktree path**: `arreo attach worker-1` lands the agent
  inside its own checkout.
- Re-spawning the same pane id **reuses** the existing worktree — a retry, a
  restart, or an operator running the command twice does not fork a second branch
  or fail on a name that is taken.

The flag is read out of the argument list before `id program [args...]` is split
apart (the same way `--socket` is), which has one consequence worth knowing: a
**program** argument spelled `--worktree` is consumed by the CLI, not passed to the
program. There is no `--` escape hatch today.

What you see, and when (the error text is the daemon's own, passed through):

| Situation | What you see |
| --- | --- |
| Worktree made or reused | `spawned <id> (worktree <name>)`, exit 0 |
| Not a git repository | `spawn: worktree: <path> is not inside a git repository (no \`git rev-parse --show-toplevel\`): <git stderr>` — refused **before** any directory is made |
| Name cannot be a directory | `spawn: worktree: "../../etc" cannot name a worktree directory: …` |
| `<root>/<name>` exists and is not this repository's worktree | `spawn: worktree: <path> exists and is not a worktree of <repo>: refusing to touch it` |
| Daemon older than this feature | `spawn: unknown request "spawn_worktree" (server speaks protocol 0)` and then `spawn: this daemon is older than the worktree feature; update it` (ADR 0017) |

A `--worktree` that silently fell back to the shared directory would be exactly the
collision the feature exists to prevent, so it is a refusal, never a fallback.

## Where the worktrees live

| Setting | Default | Effect |
| --- | --- | --- |
| `[worktree] root` | `<state>/worktrees` (`$ARREO_STATE_DIR`, else `$XDG_STATE_HOME/arreo`, else `~/.local/state/arreo`) | the directory worktrees live under, one directory per pane id: `<root>/<pane>` |
| `[worktree] repo` | the daemon's working directory | the repository a `spawn --worktree` makes its worktree from |

Both are read from the same TOML file the daemon takes with `--config` /
`$ARREO_CONFIG`:

```toml
[worktree]
root = "/srv/arreo/worktrees"
repo = "/srv/src/project"
```

The default root is deliberately **outside** the repository: inside it, every
worktree would show up in the `git status` of the very checkout the worktrees come
from. A relative or symlinked root is resolved, so the paths `git` reports and the
paths this CLI compares are the same spelling.

`arreo worktrees` reads the same section, and only the same section: `--config
PATH`, else `$ARREO_CONFIG`, else the default root. It does **not** invent a
default config path — the daemon does not have one, and two answers to "which
configuration is this machine's" is a defect this project keeps finding. Point
both at the same file.

## Branch naming

Every worktree is on branch **`arreo/<pane>`**, created from the repository's
current `HEAD`:

```console
$ git -C /srv/src/project branch --list 'arreo/*'
  arreo/worker-1
  arreo/review-pass-2
```

The prefix is what makes `git branch` read as a list of what the fleet is doing,
and it is also the association: `Entry::pane()` derives the pane id from the branch
(`arreo/worker-1` → `worker-1`), so a worktree that is not on an `arreo/` branch is
not one of ours and is not listed.

## Listing them

`arreo worktrees list` reads **`git` directly** — no daemon, no socket verb. The
worktrees are git's, not the daemon's, and the moment an operator most needs to see
them is the moment the daemon is not running. A daemon, when one answers, is asked
exactly one question: are these panes alive?

```console
$ arreo worktrees list --repo /srv/src/project --config /etc/arreo/arreo.toml
repo /srv/src/project
root /srv/arreo/worktrees
            PANE  STATE    LIVENESS  BRANCH                PATH
        worker-1  clean    live      arreo/worker-1        /srv/arreo/worktrees/worker-1
   review-pass-2  dirty    exited    arreo/review-pass-2   /srv/arreo/worktrees/review-pass-2
         old-run  MISSING  exited    arreo/old-run         /srv/arreo/worktrees/old-run
```

- **STATE** is `clean`, `dirty`, or `MISSING`. `MISSING` is a worktree git calls
  *prunable* — the directory is gone. It is marked and never silently dropped:
  a worktree that vanished is precisely what has to be noticed.
- **LIVENESS** is `live` or `exited`, from the daemon. **When no daemon answers the
  column is absent and one line says so** — on stderr, in both output modes, so
  stdout stays a listing and nothing else — because the listing is still useful
  without it:

  ```console
  $ arreo worktrees list --repo /srv/src/project
  repo /srv/src/project
  root /srv/arreo/worktrees
              PANE  STATE    BRANCH                PATH
         worker-1  clean    arreo/worker-1        /srv/arreo/worktrees/worker-1
  (no daemon: liveness unknown)
  ```

  `--repo` defaults to the `[worktree] repo` of `--config`/`$ARREO_CONFIG`, and
  to the current directory when neither says (resolved with `git rev-parse
  --show-toplevel`, so a subdirectory works). The configuration is consulted
  because the **daemon** resolves a pane's worktree against it: a consumer that
  went straight to its own working directory would look in a different repository
  and report "no worktree" about a pane that has one. A path that is not a
  repository is refused, exit 2, naming the path.

`--json` is the script contract; the table above is not one and may change.

```console
$ arreo worktrees list --repo /srv/src/project --json --socket /run/user/1000/arreo.sock
{"schema":1,"worktrees":[{"pane":"worker-1","path":"/srv/arreo/worktrees/worker-1","branch":"arreo/worker-1","dirty":false,"missing":false,"live":true}]}
```

- `schema` is `1`; the shape is additive-only.
- **`live` is `null` when no daemon answered** — the same honesty rule `arreo
  machines` follows (T-0044): never invent a value. `true`/`false` come from the
  daemon's own pane list, so a pane it does not know about is `false` (exited), not
  a guess.
- `missing` is the `prunable` flag; a `MISSING` row's `dirty` is `false` because
  there is no working tree left to be dirty — read `missing`, not `dirty`.

## Removing one

```console
$ arreo worktrees remove worker-1 --repo /srv/src/project
removed /srv/arreo/worktrees/worker-1
branch arreo/worker-1 kept (the commits are on it; `git branch -D arreo/worker-1` deletes it)
```

**The branch is kept.** A worktree is a *checkout* of the branch; the commits are on
the branch. "Stop working here" and "throw the commits away" are different
decisions, so the second one is `git branch -D` and never this command. The output
says where the commits are so an operator does not have to guess.

### The two rules

1. **A dirty worktree is never deleted.** Uncommitted work is the only
   unrecoverable thing in this feature, so the refusal names the files:

   ```console
   $ arreo worktrees remove review-pass-2 --repo /srv/src/project
   worktrees remove: /srv/arreo/worktrees/review-pass-2 has uncommitted changes: src/main.rs, NOTES.md — commit, stash or `--force`
   $ echo $?
   1
   ```

   `--force` removes it and discards that work. Nothing else does.

2. **A live pane's worktree is not removed without `--force`.** That agent is
   working *there*, right now:

   ```console
   $ arreo worktrees remove worker-1 --socket /run/user/1000/arreo.sock
   worktrees remove: worker-1 is live; that agent is working in this worktree (--force removes it anyway)
   $ echo $?
   1
   ```

   With `--force` it goes, and the fact that it was live is still said:

   ```console
   $ arreo worktrees remove worker-1 --force
   removed /srv/arreo/worktrees/worker-1
   branch arreo/worker-1 kept (the commits are on it; `git branch -D arreo/worker-1` deletes it)
   worktrees remove: worker-1 is live; removing anyway (--force)
   ```

   **Liveness is the one question git cannot answer.** With no daemon reachable the
   removal proceeds and says so — `no daemon is reachable, so liveness is unknown;
   removing anyway` — rather than refusing, because a checkout left behind by a
   long-dead daemon is the ordinary case for this command.

Other refusals, each naming the pane or the path: a worktree that is `MISSING`
(nothing to remove; `git worktree prune` clears the stale entry), a directory under
the root that is not this repository's worktree (refused, never touched), and a
pane whose worktree is registered under a *different* root than the configured one
(the message names where it actually is — usually a `[worktree] root` that changed
after it was made). Removing a pane that has no worktree at all is not an error:
`no Arreo worktree for <pane> at <path>`, exit 0.

## Daemon restart

Worktrees are not tied to the daemon's lifetime, and a restart is where that
matters:

- A pane that was spawned with `--worktree` **comes back in its own worktree**:
  the record's pane id is validated against the configured root and the checkout is
  re-made as `<root>/<pane>` — it is the same checkout, on the same branch, with the
  work still in it.
- If the directory was **deleted while the daemon was down**, the worktree is
  re-created on its branch — the branch kept the commits, which is why removal never
  deletes it.
- If the path now holds a **directory that is not a worktree of that repository**,
  the pane is skipped with a loud stderr line naming the id and the reason. Starting
  it in the wrong directory is the one outcome worse than not starting it.
- **A record from a different root is refused, not repaired.** The root is the one
  `[worktree] root` configures *now*, never the one the record names: a record is a
  file your own uid can edit, and the restore path *creates* directories, so a root
  taken from a record would let it make one anywhere. A record whose path is not
  `<configured root>/<pane>` is therefore skipped with a line naming the record, the
  path and the configured root — and nothing is created. Point `[worktree] root` back
  at the directory its checkout is in, or spawn the pane again; the alternative
  (silently re-creating it under the new root) would move the agent to a different set
  of files while the work it had left behind sat in the old place.
- A pane that exits has its worktree removed **only when it is clean**; a dirty one
  is kept and reported. `arreo worktrees list` is how you see what was kept.

## See also

- `arreo spawn` and `arreo worktrees` in the top-level `arreo` usage text.
- [docs/tour.md](tour.md) for where state lives and how the daemon fits.
- `specs/adr/0017-protocol-n-minus-1.md` for the N−1 wire-compatibility rule that
  makes an old daemon refuse `spawn --worktree` by name instead of silently
  spawning in the shared directory.
