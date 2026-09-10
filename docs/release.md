# Release process (T-0020 skeleton — first real release fills the blanks)
#
# 1. Tag: `git tag v0.1.0 && git push origin v0.1.0` (CI `release` job runs
#    cargo-dist, builds the 3 targets, uploads artifacts + installers).
# 2. Installers: `curl -fsSL https://arreo.dev/install.sh | sh` and the
#    PowerShell twin (placeholders until arreo.dev serves them — the URLs
#    below are RESERVED, not live).
# 3. Signing (before revenue, per the pre-revenue audit gate): Sigstore
#    (`cargo dist sign` / cosign) or minisign; checksums published alongside.
#    Until then: artifacts are UNSIGNED — never present them as trusted.
# 4. Verify: `cargo xtask package --dry-run` (plan mode) on every PR proves
#    the manifest parses; real builds happen on tags only.
#
# Install URLs (reserved):
# - sh:         https://arreo.dev/install.sh
# - PowerShell: https://arreo.dev/install.ps1
# - Homebrew:   brew install arreo/tap/arreo
