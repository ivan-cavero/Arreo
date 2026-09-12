# Release process (T-0020 skeleton — first real release fills the blanks)

> **Status: unexecuted.** Arreo is pre-launch. There are no releases, no tags, no
> published crates, no signed artifacts, and the `arreo.dev` URLs below are
> **reserved names, not working endpoints** — the domain has no DNS record. Nothing in
> this file has been run end to end; it is the procedure a first release would follow,
> with each unresolved step marked as unresolved rather than implied.
>
> The one part with a machine check behind it is the manifest: `cargo xtask package
> --dry-run` runs in CI on every PR and fails if the cargo-dist skeleton stops parsing
> or loses a target.

1. **Tag.** `git tag v0.1.0 && git push origin v0.1.0`. The cargo-dist wiring is
   declared in `[workspace.metadata.dist]` (`cargo-dist-version`, the three targets,
   the two installers) and validated by `cargo xtask package --dry-run` — but a
   tagged release workflow does not exist in `.github/workflows/` yet, so tagging
   today builds and publishes nothing. Landing that workflow is part of the first
   real release, not of this document.
2. **Installers.** `curl -fsSL https://arreo.dev/install.sh | sh` and the PowerShell
   twin. Neither is live: the domain does not resolve, and the URLs below are
   reserved placeholders. cargo-dist would generate both.
3. **Signing** (before revenue, per the pre-revenue audit gate): Sigstore
   (`cargo dist sign` / cosign) or minisign, with checksums published alongside.
   Until then artifacts are **UNSIGNED** — never present them as trusted.
4. **Verify.** `cargo xtask package --dry-run` runs on every PR (plan mode) and proves
   the manifest parses and still names the expected targets and installers. Real builds
   happen on tags only.

## Updating a client in place

While there is no release channel, a client can still be updated from a binary you
already have (T-0070):

```console
$ arreo update --from /path/to/a/newer/arreo
installed /home/you/.cargo/bin/arreo
version: arreo 0.1.0
previous kept at /home/you/.cargo/bin/arreo.prev
resumed pane build from /run/user/1000/arreo.sock (4 line(s) after 0)

$ arreo update --rollback          # put the previous binary back
$ arreo update --check             # refused: this build has no channel (see below)
```

### The invariant

**The client update path never signals, reaps, restarts or stops a PTY-bearing
process, and never stops the daemon.** It touches exactly three things: the binary
path, the `.prev` sibling, and the resume token. The daemon owns the agents; this
verb owns the client binary. That is what makes a client restart cost seconds and
touch nobody else, and it is asserted — not promised — by `cargo xtask e2e --slice
update`, which holds a real daemon and eight live panes across a real update and
checks that no pid moved and no pane's output stream restarted.

### What happens, in order

```text
1. write the resume token      (before the swap: a swapped client that cannot say
                                where it was has lost the operator's place)
2. copy the candidate to       (same directory ⇒ same filesystem ⇒ the rename
   <binary>.staged              below is atomic)
3. hard_link the current       (a second name for the old inode; nothing moves)
   binary to <binary>.prev
4. rename(<binary>.staged,     (atomic replace, one syscall)
   <binary>)
5. hand over to the new        (exec with the same arguments; the second run finds
   binary                       the binary already installed and resumes)
```

There is no instant at which the binary path is missing or non-executable, and a
crash between any two steps leaves a runnable binary there — the old one before
step 4, the new one after. The reasoning, and the rejected two-rename design, are
in [ADR 0020](../specs/adr/0020-client-update-swap.md).

### What `--check` refuses

`arreo update` with no `--from` is the **anonymous** path: fetch a release and
verify its signature before staging it. That needs the signing key (T-0036), and
this build has no channel to check — so `--check` says so and exits 2 rather than
pretending. Installing an artifact nobody verified is the one thing an updater
must never do quietly.

### When the path belongs to a package manager

Homebrew and `cargo install` own their binary paths. The updater does not fight
them: a path it cannot write produces the command that does the job
(`brew upgrade arreo`, `cargo install --force arreo`) instead of a partial write or
a `sudo` over the package manager's files.

## Install URLs (reserved — none of these resolves today)

| Channel | URL | State |
| --- | --- | --- |
| Shell | `https://arreo.dev/install.sh` | Reserved; domain has no DNS record |
| PowerShell | `https://arreo.dev/install.ps1` | Reserved; domain has no DNS record |
| Homebrew | `brew install arreo/tap/arreo` | The `arreo/homebrew-arreo` tap named in `[workspace.metadata.dist]` does not exist |

Until a release exists, the only install path is building this checkout:
`cargo build --workspace` ([docs/tour.md](tour.md) has the walkthrough).
