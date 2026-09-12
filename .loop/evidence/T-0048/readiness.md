# T-0048 readiness handoff — the human gate
================================================

**This task ends `needs-human`. The loop never flips visibility, creates the org, or announces.**

## What was proven, and where

| Criterion | Verdict | Artifact |
| --- | --- | --- |
| License story (Apache/AGPL split, dep direction, manifests) | PASS (gate + hand-check) | `release-check: license PASS`; `license-check.txt` |
| REUSE coverage | FIXED in-tree; real `reuse lint` still needs CI | `REUSE.toml` (dead paths removed, license texts split); `release-check: reuse SKIP` locally |
| Stranger path (clean clone → first pane → green slice) | PASS | `stranger-tour.txt` (build 1m39s, api slice green) |
| Newcomer docs + link/command integrity | PASS (123 rows, 0 missing) | `docs/tour.md`; `docs-check.txt` |
| Repo furniture (3 issue forms, PR template) | DONE, YAML-valid | `.github/ISSUE_TEMPLATE/*`, `.github/PULL_REQUEST_TEMPLATE.md`, `furniture.md` |
| SECURITY.md disclosure process | DONE, honest about the gap | `SECURITY.md` (states PVR is disabled + the flip instruction) |
| Secrets scan (whole history) | 0 findings with triaged allowlist | `secrets-scan.txt`, `secrets-scan.json`, `.gitleaks.toml` |
| Public-claims audit (137 claims) | FALSE claims fixed in place | `claims-audit.md`; README/CONTRIBUTING/NOTICE/docs edits |
| Automated gate | DONE, in CI | `cargo xtask release-check [--public]`; `public-readiness` job |
| CI matrix green | **NOT YET** — see below | T-0063 (p1, filed) |

## The two things only a human can do

1. **Read the ubuntu test log.** CI needs admin rights this box does not have; the ubuntu
   `test` leg fails in ~2 min (exit 101) while fmt/build/clippy pass. T-0062 (Windows) and
   the macOS clippy failure are fixed in-tree; the ubuntu failure is unidentified. T-0063
   carries the full diagnosis.
2. **Decide the launch itself.** The repo is *already public* (`ivan-cavero/Arreo`,
   public since 2026-09-10) but serves the OLD README/NOTICE/CONTRIBUTING on `main` —
   landing this batch replaces them with the honest versions. The exact command from the
   task (`gh repo edit --visibility public`) is therefore already true; what remains is
   the announcement checklist:
   - [ ] Flip private vulnerability reporting (SECURITY.md names the setting + API call)
   - [ ] Confirm the "Report a vulnerability" button on the Security tab (logged-in browser)
   - [ ] Watch one full CI run green (needs T-0063 first)
   - [ ] Announce

## What changed in this batch (for the reviewer)

- `xtask/src/release_check.rs` (new) + CI `public-readiness` job + `.gitleaks.toml`
- `SECURITY.md`, issue/PR templates, `docs/tour.md`, README/CONTRIBUTING/NOTICE rewrites
- `REUSE.toml` fixes + the workspace_deps gate's continuation-line blind spot
- `Cargo.toml` repository URL, CODE_OF_CONDUCT contact
- T-0062 (Windows build, done), T-0063 (CI investigation, p1, open)
