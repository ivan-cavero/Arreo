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

## Install URLs (reserved — none of these resolves today)

| Channel | URL | State |
| --- | --- | --- |
| Shell | `https://arreo.dev/install.sh` | Reserved; domain has no DNS record |
| PowerShell | `https://arreo.dev/install.ps1` | Reserved; domain has no DNS record |
| Homebrew | `brew install arreo/tap/arreo` | The `arreo/homebrew-arreo` tap named in `[workspace.metadata.dist]` does not exist |

Until a release exists, the only install path is building this checkout:
`cargo build --workspace` ([docs/tour.md](tour.md) has the walkthrough).
