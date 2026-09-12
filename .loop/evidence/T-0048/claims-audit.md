# Public-claims audit — T-0048 criterion 6

Auditor: ClaimsLicense (T-0048 worker) · 2026-09-12T02:17Z–02:40Z
Repo under audit: `/home/dev/dev/Arreo`, working tree on git `4c249cc`
Live remote: `https://github.com/ivan-cavero/Arreo` (`git remote -v` → origin)

## 0. Method, and the one fact that reframes everything

**The repository is already public.** As of this audit, GitHub's API returns
`private: false`, `visibility: public`, `created_at: 2026-09-10T14:17:48Z`,
`pushed_at: 2026-09-12T01:57:46Z`, default branch `main`, license detected as
Apache-2.0. `tasks/T-0048-oss-launch-readiness.md` still lists the visibility flip as a
**HUMAN GATE** ("the loop never flips visibility, creates the org, or announces") — but
that gate has already been passed, and `main` has been serving the *old* README.md,
NOTICE.md and CONTRIBUTING.md the whole time. So this audit is not "check the staging
copy": it is **what a stranger reads right now**, plus what this batch will replace it
with. Both are reported below.

Sources (all re-checked in this session):
- local working tree (hashed; `sha256` of each audited file recorded at the end);
- `raw.githubusercontent.com/ivan-cavero/Arreo/main/<file>` for the live public text;
- GitHub REST API: repo metadata, releases, tags, Actions runs/jobs/annotations,
  `private-vulnerability-reporting`;
- DNS (`getent hosts arreo.dev`, `app.arreo.dev`), HTTPS status for every non-`arreo.dev`
  URL cited in the audited files, and `crates.io`'s API for the crate badge;
- the code: `crates/**`, `xtask/**`, the manifests, `Cargo.lock`, `perf-budget.toml`,
  `deny.toml`, `REUSE.toml`, `.github/workflows/*`, `.loop/evidence/**`.

Verdict vocabulary: **TRUE** (holds today, with the artifact cited), **FALSE** (does not
hold today), **UNVERIFIABLE** (nothing in or in front of this repo can settle it).
Console transcripts inside docs are judged on their **commands and stated effects**, not on
the prose of their sample output; where the sample output itself is a claim (a column that
does not exist, a count the directory cannot hold) it is judged too.

**Counts: 137 claims audited — 129 TRUE, 2 FALSE, 6 UNVERIFIABLE** (per-file arithmetic in
§9), **plus 23 false claims live on `main` right now** (the pre-batch README/NOTICE/
CONTRIBUTING, §2), which pushing this batch removes. Of the 19 defect rows found at
snapshot, siblings fixed 17 during this audit (verified by re-read, §9b); the two
remaining open defects are D32's underlying cause (CI itself is still red) and O1
(`CODE_OF_CONDUCT.md:33` still prints a bouncing address with no pre-launch note). The
headline defects, in severity order:

1. **CI has never passed.** All 90 `ci` runs since the repo went public
   (2026-09-10T15:00Z → 2026-09-12T01:57Z) **failed**; zero successes. The current HEAD
   (`4c249cc`) fails on all three OSes — ubuntu `test (incl. dependency-direction gate)`
   exit 101, windows `build` exit 1, macos `clippy (zero warnings)` exit 1 —
   run `https://github.com/ivan-cavero/Arreo/actions/runs/34666329483` (job logs need
   auth; the failing step names come from the public jobs API). Every doc sentence that
   leans on CI ("CI builds and tests all three", "the nightly run uploads measured
   numbers", "real runners prove it behaves") is therefore false today, in **both** the
   live and the rewritten text.
2. **The live public README is the old one.** `raw` fetches show 311 lines of the
   pre-rewrite README on `main` (release badge, `v1.0.0`, `arreo server init`,
   `curl https://arreo.dev/install.sh | sh`, "security audited" badge, nine themes,
   plugins, mobile/web) — and the old NOTICE.md (34 lines, SBOM URL + `docs/forking.md`)
   and old CONTRIBUTING.md (167 lines, `cargo xtask setup`, `themes/template.json`,
   `conduct@arreo.dev` with no pre-launch note). `SECURITY.md` → HTTP 404 on `main`
   while local CONTRIBUTING links to it.
3. **`arreo.dev` does not exist**: `getent hosts arreo.dev` → no record,
   `app.arreo.dev` → no record. Every URL on that domain in the live text is dead.
4. **Badges point at the wrong repo**: the live README's release/CI badges target
   `github.com/arreo-dev/arreo` (HTTP 404) while the actual remote is
   `ivan-cavero/Arreo`. The crates.io badge targets a crate that does not exist
   (`api/v1/crates/arreo-server` → 404 `crate does not exist`).
5. **No release, no tag, no crate** — GitHub releases API: 0; GitHub tags API: 0;
   `git tag`: 0; workspace version `0.1.0`. Any `v1.0.0` wording is false.
6. **Three stale technical claims sit on `main` today** (and are unchanged locally):
   relay schema "3" vs code 5, machine audit schema "6" vs code 7, and four places
   asserting relay presence/durable audit rows "do not exist" when both ship
   (§7).
7. **Discussions do not exist**: API `has_discussions: false`,
   `github.com/ivan-cavero/Arreo/discussions` → 404, while the new CONTRIBUTING calls
   Discussions "the only discussion surface that exists today". Issues *are* enabled
   (`has_issues: true`), so the issue-tracker fallbacks do work.
8. `REUSE.toml` maps three paths that are not in the repository (`mobile/**`,
   `web/**`, `themes/**`) — detail in `.loop/evidence/T-0048/license-check.txt` §4.

---

## 1. README.md — current working tree (post-rewrite, not yet pushed)

| # | Claim | Verdict | Evidence / required change |
| --- | --- | --- | --- |
| R1 | "**Status: pre-launch and pre-1.0.** There is no release, no published crate, no installer and no `arreo.dev` — the domain does not resolve." | TRUE | releases/tags APIs 0; `crates.io` 404 for `arreo-server`; DNS NXDOMAIN. |
| R2 | "Everything below was run against this tree" | UNVERIFIABLE | No artifact maps statement→run. Recorded runs exist (`.loop/evidence/T-0008/bench-10.txt`, `T-0014/verify.log`, `T-0021/demo.json`, per-task `verify.log`s) but nothing ties them to the page. Weaken to "the commands in this file are the ones in `AGENTS.md` and `docs/tour.md`", or point at `.loop/evidence/`. |
| R3 | "Clients: `arreo` (CLI), `arreo-tui` (ratatui terminal UI)." | TRUE | both binaries exist (`crates/arreo-cli/Cargo.toml`, `crates/arreo-tui/Cargo.toml`). |
| R4 | "The runtime is Apache-2.0; the relay is AGPL-3.0-or-later." | TRUE | license-check.txt §2; GitHub also detects `apache-2.0`. |
| R5 | "Work survives its UI closing \| Yes — the daemon owns the PTYs, not the client." | TRUE | `arreo-server` owns panes; e2e `lifecycle`/`persistence` slices; `.loop/evidence/T-0018`. |
| R6 | "Work survives the daemon crashing \| Yes — panes and scrollback restore from the sidecar store on restart." | TRUE | `T-0018` persistence slice (`kill -9` restore), evidence dir present. |
| R7 | "Semantic agent state \| `working / idle / question / blocked / done`, with the pattern or event that decided it, and an honest `inferred` vs `direct` label." | TRUE | `AgentState` in `arreo-core::proto`; `docs/agent-skill.md` shows `confidence=inferred:silence+prompt-shape`; `T-0014` executed the skill doc verbatim. |
| R8 | "Per-agent resources \| Live RSS/CPU per pane tree, plus a durable metrics history." | TRUE | metrics sampler + `arreo metrics history --since/--step`. |
| R9 | "An audit trail \| Append-only, secrets redacted at write time." | TRUE | `docs/audit.md` §1/§4; store exposes no update/delete for `audit`. |
| R10 | "`git clone https://github.com/ivan-cavero/Arreo && cd Arreo`" | TRUE | HTTP 200, public, default branch `main`, matches `origin`. |
| R11 | "Requires stable Rust, pinned by `rust-toolchain.toml`." | TRUE | file exists. |
| R12 | "Building from source *is* the install — there is no package, no tap and no installer." | TRUE | no tag/release/tap; `Cargo.toml` has only the cargo-dist *skeleton*. |
| R13 | "Linux, macOS and Windows are build targets; CI builds and tests all three (`.github/workflows/ci.yml`)." | **FALSE at snapshot → FIXED during this audit** | The workflow configures the three-OS matrix, but **0 of 90 runs had passed** and the current HEAD was red on all three legs. A sibling has since stated the red state in place (verified re-read: "is currently red on all three legs"). CI itself still needs fixing. |
| R14 | "Binaries land in `target/debug/`: `arreo-server`, `arreo`, `arreo-tui`, `arreo-relay`" | TRUE | `[[bin]]` in each crate's manifest. |
| R15 | First-pane transcript (`./target/debug/arreo-server &` → `arreo spawn build /bin/sh` → `arreo read build` → `arreo-tui` → `arreo send` → `arreo wait … --state idle` → `arreo server stop`) | TRUE | every verb/flag is in the CLI's dispatch (`main.rs` usage + match); states/durations parse (`parse_duration_ms`, `500ms/30s/5m`). Sample output prose not judged. |
| R16 | "`arreo wait build --state idle --timeout 30s`" → "state=Idle confidence=inferred:silence pattern=None" | TRUE | `cmd_wait`; the `inferred:silence` label and `pattern=` field are real (`T-0014` log shows the same shape for `Question`). |
| R17 | "`arreo service install` writes the unit for this machine — a systemd user unit, a launchd agent, or a `sc.exe` script on Windows — and enables it where the platform allows." | TRUE | `arreo_core::lifecycle::unit_path`; `cmd_service install`; e2e `lifecycle` slice does the service round-trip. Windows path is code-only (Windows CI is red) — worth a one-clause hedge later if CI stays red. |
| R18 | "What the daemon can do today" table: `--version`, `panes`, `spawn`, `read`, `attach`, `send`, `wait`, `split`, `metrics`, `metrics history`, `audit`, `audit export\|prune`, `devices id\|list\|issue\|rotate\|revoke\|authorize`, `pair`, `pair --join`, `machines list\|status\|rename\|remove\|add\|trust`, `attach --machine`, `service install\|uninstall\|status`, `server stop` | TRUE | every one is in the CLI's usage/dispatch; `machines` sub-verbs verified in `machines.rs`; `devices` sub-verbs in `main.rs`. |
| R19 | "`arreo panes` # every pane: id, state, alert" | TRUE | `cmd_panes` prints header `ID  STATE  ALERT` (alerts sort first, T-0041). |
| R20 | "`panes`, `read`, `send`, `wait`, `split`, `metrics` and `attach` all take `--machine <name>`" | TRUE | usage block lists exactly those verbs with `--machine`; `docs/machines.md`. |
| R21 | "`arreo-tui --machine <name>` opens that machine's panes" | TRUE | `crates/arreo-tui/src/main.rs` parses `--machine`. |
| R22 | "States are `working\|idle\|question\|blocked\|done\|unknown`; durations are `500ms`, `30s`, `5m`." | TRUE | `AgentState`; `parse_duration_ms` doc comment names exactly those forms. |
| R23 | "recorded per-harness adapters (`adapters/*.toml` — `opencode` and `pi` have native tiers, `default` is the universal one)" | TRUE | `adapters/{default,opencode,pi}.toml` + `{opencode,pi}-native.md`. |
| R24 | "five ship in the binary: `arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system`" | TRUE | `BUILTINS` in `crates/arreo-core/src/theme/loader.rs` (5 entries; files in `crates/arreo-core/themes/`). |
| R25 | "discovered from `$XDG_CONFIG_HOME/arreo/themes` and `./.arreo/themes`" | TRUE | `theme::loader::default_dirs()` — also `$ARREO_THEME_DIR` and `<project>/.arreo/themes`; the two named are a true subset. |
| R26 | "explicit 24-bit colour with an honest depth fallback" | TRUE | `theme::color` depth quantization; e2e `theme` slice. |
| R27 | "What is not here yet" — 8 bullets (no releases/artifacts, no mobile/web, no config sync, no auto-update/live handoff, no plugin runtime, no push/approvals, no hosted service/pricing, Linux-only enforcement) | TRUE | each verified by absence: 0 tags; no `mobile/`,`web/`; no sync/update/plugin/approval/push code in `crates/`; `EnforceError::Unimplemented` for non-Linux (`crates/arreo-core/src/enforce/other.rs`); `arreo-plugin-api/src/lib.rs` is a 2-line shell. |
| R28 | "The daemon binds a Unix socket and opens no network port." | TRUE | `default_socket()`; the only QUIC listener is behind the `ARREO_TRANSPORT_TEST_LISTEN` test seam (`crates/arreo-server/src/transport.rs`). |
| R29 | "the relay routes bytes it cannot read" | TRUE | `docs/relay-protocol.md` §8.3; relay schema test forbids content columns. |
| R30 | "Pairing is a four-word code … SPAKE2 … certificate signed by the account root … each machine keeps its **own** trust ledger" | TRUE | `spake2` dep + `arreo pair`; `identity/authority`; per-machine trust (docs/machines.md, ADR 0019). |
| R31 | "`cargo test -p xtask --test workspace_deps` fails the build if that direction is ever reversed" | TRUE | test exists and asserts exactly that (read; not run). **Note:** with the current red CI it cannot gate anything. |
| R32 | "`cargo xtask release-check` re-reads the declared license of every crate" | TRUE | `xtask/src/release_check.rs::license_item`. |
| R33 | Security section, 7 bullets (no inbound port; SPAKE2 one-guess; relay cannot read; per-machine revocation + `--machine` grants write audit rows; append-only-by-API but **not** tamper-evident; nothing is signed yet; no external review and SECURITY.md says so) | TRUE | each verified in code/docs; the tamper-evidence and unsigned-artifact admissions match `docs/audit.md` §1 and `docs/release.md`; "no external review" matches SECURITY.md "Posture on audits". |
| R34 | "`perf-budget.toml` is the law … Rows marked `phase0 = true` are asserted today" + 7-row budget table (100 MB/10 panes, 3 MB/pane, 200 ms, 300 ms sweep, 50 ms VT, byte-exact replay, 20 MB binary) | TRUE | all seven are `phase0 = true` in `perf-budget.toml` with those targets; `bench` evidence `.loop/evidence/T-0008/bench-10.txt` (10 MB RSS, 211 KB/pane, 0 ms detection…). |
| R35 | "Measured numbers are uploaded as artifacts by the nightly run (`.github/workflows/nightly-bench.yml`) on Linux, macOS and Windows" | **FALSE at snapshot → FIXED during this audit** | The workflow exists with a three-OS matrix and `upload-artifact`, but of the 90 recorded runs exactly **one** was `nightly-bench`, and it **failed**: no artifact exists. A sibling has since rewritten the sentence in place (verified re-read: "is written to run that bench … but **it has not succeeded yet** — the one recorded run failed — so there is no artifact to point at"). |
| R36 | "The 30-pane, ≤ 120 MB row is recorded and **not** enforced yet — the Phase 0 proof point is 10 panes." | TRUE | `server_rss_30panes_mb = { phase0 = false }`, `server_rss_10panes_mb = { phase0 = true }`. |
| R37 | Documentation table (docs/tour.md, AGENTS.md, ROADMAP.md, docs/*.md, specs/adr/) | TRUE | every relative link resolves (hand-check; `xtask`'s links item checks the same set minus AGENTS/ROADMAP). |
| R38 | "the repo is developed in agent loops" (linked to `ROADMAP.md#10-…`) | TRUE | ROADMAP §10 exists; the anchor matches that heading; `tasks/` + `.loop/evidence/` are in-tree. |
| R39 | License section (dual license table, the four "what that means" bullets, SPDX line, REUSE.toml, NOTICE.md) | TRUE | license-check.txt §1–§3. |
| R40 | Acknowledgments: "`[rust-noise]` for handshakes", defined as `https://github.com/iqlusioninc/crates/tree/main/noise-framework` | **FALSE at snapshot → FIXED during this audit** | `noise`/`noise-framework` appears **0 times** in `Cargo.lock`; the Noise implementation is `snow` 0.9.6 (`crates/arreo-core/Cargo.toml`, `transport/noise.rs` → `Noise_KK_25519_ChaChaPoly_BLAKE2s`). A sibling has since changed the acknowledgment to `[snow]: https://github.com/mcginty/snow` (verified re-read). |

## 2. README.md / NOTICE.md / CONTRIBUTING.md **as served on `main` right now**

These are not hypotheticals: `raw.githubusercontent.com/ivan-cavero/Arreo/main/...` returns
them today (311/34/167 lines). They are listed so the planner can see what a stranger reads
before the batch lands; the fix is to land the rewritten files.

| # | Live claim | Verdict | Evidence |
| --- | --- | --- | --- |
| L1 | `[![release](…/github/v/release/arreo-dev/arreo…)]` badge | FALSE | repo `arreo-dev/arreo` → 404; 0 releases/tags anywhere. |
| L2 | `[![CI](…/arreo-dev/arreo/ci.yml…)]` badge | FALSE | wrong repo in the URL, and the real CI is 90/90 red. |
| L3 | `[![crates.io](…/crates/v/arreo-server)]` badge | FALSE | crates.io API: "crate `arreo-server` does not exist". |
| L4 | `[![security audited](…externally%20audited…)](https://arreo.dev/security)` | FALSE | no audit exists (SECURITY.md, ROADMAP §4/§6); the URL does not resolve. |
| L5 | "Thirty agents across three machines … Your phone pings you when one is blocked on a question." | FALSE | no mobile client, no push (verified by absence); `mobile/`,`web/` do not exist. |
| L6 | `curl -fsSL https://arreo.dev/install.sh \| sh` (hero + quickstart, and the PowerShell twin) | FALSE | domain has no DNS record; `docs/release.md` itself marks the URLs RESERVED. |
| L7 | `arreo server init` (quickstart steps 2) | FALSE | no `init` verb; `arreo server stop` and `arreo service install` are the real surface. |
| L8 | bare `arreo` with the comment "# attach the TUI" | FALSE | bare `arreo` prints usage (exit 2); the TUI is `arreo-tui`. |
| L9 | `arreo spawn claude "refactor the auth module"` | FALSE | usage is `spawn <id> <program> [args...]`. |
| L10 | `arreo machines` + an example table with per-machine "12 agents · 2 blocked" | FALSE | no sub-verb given (usage required); and the directory is metadata-only — it holds no agent counts (docs/machines.md; `print_table` prints name/presence/last-seen/conflict). |
| L11 | `arreo attach workbox` | FALSE | correct form is `arreo attach --machine workbox [<pane>]`. |
| L12 | `arreo update` performing a live handoff, and the "Updates that never break your flow" section | FALSE | no `update` verb, no handoff code (the new CONTRIBUTING states this as a gap). |
| L13 | `arreo plugin new token-meter`, hot-load from `~/.config/arreo/plugins/`, "Registry and signing at arreo.dev/plugins" | FALSE | no plugin verb, no WASM runtime, dead URL. |
| L14 | `arreo agent state <id> question --text "…"` | FALSE | no `agent` verb. |
| L15 | "[sync] watch = […]" config-sync section | FALSE | no sync code, no `arreo sync`. |
| L16 | "Nine built-ins (… nord, everforest, ayu, one-dark, system)" | FALSE | 5 built-ins in code. |
| L17 | Socket API list "… `metrics` · `machines`" | FALSE | `machines` is a relay control kind, not a socket verb (`role::Verb` = Hello/Panes/Read/Attach/Wait/Metrics/Send/Spawn/Split/Kill/Admin). |
| L18 | Pricing table + "Details at arreo.dev/pricing" | FALSE | no product, no domain (the new README correctly defers this to the roadmap). |
| L19 | Documentation list of nine `arreo.dev` URLs | FALSE | NXDOMAIN. |
| L20 | NOTICE.md: "A complete, machine-generated inventory … ships with every release at `https://arreo.dev/notices/<version>`" | FALSE | no releases, no domain, no SBOM artifact — I fixed this in the working tree. |
| L21 | NOTICE.md: "Forks must rename: see `docs/forking.md`" | FALSE | no such file (docs/ has 8 + tour.md) — fixed in the working tree. |
| L22 | NOTICE.md: rust-noise/uniffi/wasmtime rows as present dependencies | FALSE | none in `Cargo.lock` — fixed in the working tree. |
| L23 | CONTRIBUTING.md (old): `cargo xtask setup`, `themes/template.json`, `cargo xtask adapters --check myharness`, `cargo xtask plugin new` | FALSE | none of those verbs/paths exist (the rewritten CONTRIBUTING removes all four — verified). |

## 3. CONTRIBUTING.md — current working tree (post-rewrite)

| # | Claim | Verdict | Evidence / required change |
| --- | --- | --- | --- |
| C1 | "the `conduct@arreo.dev` address in that file is pre-launch: the `arreo.dev` domain does not resolve yet, so it bounces." | TRUE | DNS NXDOMAIN. **But `CODE_OF_CONDUCT.md` itself (line 33) still prints the address with no pre-launch note** — route that one-line fix (file not owned by me). |
| C2 | "`git clone https://github.com/ivan-cavero/Arreo`" | TRUE | HTTP 200 / public / `main`. |
| C3 | `cargo build --workspace`, `cargo test --workspace`, `cargo xtask e2e` | TRUE | commands documented in `AGENTS.md`; the bare-`e2e`-is-a-stub caveat is stated 10 lines later in the same section. |
| C4 | Windows notes (Win10 1809+, Developer Mode for symlinked fixtures, UTF-8 codepage) | TRUE | `docs/conpty-windows.md` (forced `chcp 65001` in the smoke; conhost degradation path). |
| C5 | "`cargo xtask e2e --slice lifecycle` exercises the service units the way the OS's own manager would" | TRUE | `xtask/src/lifecycle_slice.rs` header: "service install round-trip + SIGTERM drain". |
| C6 | "CI runs the whole battery on Linux, macOS and Windows (`.github/workflows/ci.yml`)." | **FALSE at snapshot → FIXED during this audit** | 90/90 runs failed; the current HEAD was red on all three legs. A sibling has since stated the red state in place (verified re-read: "It is red on all three legs today"). CI itself still needs fixing. |
| C7 | "The supply-chain gates need three installed tools (`cargo vet`, `cargo audit`, `cargo deny`) — the CI versions are pinned in that workflow." | TRUE | `ci.yml` installs `cargo-vet 0.10.0`, `cargo-deny 0.20.2`, `cargo-audit 0.22.0`. |
| C8 | The 8 everyday commands (`e2e --slice`, `bench`, `conpty-smoke`, `check-targets`, `adapters --check`, `release-check`, `clippy -D warnings`, `audit/vet/deny`) | TRUE | each verb exists in `xtask/src/main.rs` / `AGENTS.md`. |
| C9 | "The real e2e slices are `chaos`, `api`, `compat`, `lifecycle`, `persistence`, `state`, `enforcement`, `tui`, `theme` and `relay`" | TRUE | `e2e()` dispatch lists exactly those. |
| C10 | "**Bare `cargo xtask e2e` with no `--slice` is still a stub that prints `not implemented` and exits 0**, so it is not evidence" | TRUE | `stub()` in `xtask/src/main.rs`. |
| C11 | "the `enforcement` slice reports honestly whether this box can delegate a cgroup rather than pretending it passed" | TRUE | `enforcement_slice`; `ci.yml`'s ubuntu-only cgroup step + comment. |
| C12 | Rule 2: "Your PR must pass the slices it touches on Linux, macOS, *and* Windows — CI runs the whole battery on all three" | **FALSE at snapshot → FIXED during this audit** | same as C6; a sibling has since added the red-state caveat in place (verified re-read). The rule is fine as policy; as a description of the repo it was false until stated. |
| C13 | Rule 3: "Every row in `perf-budget.toml` carries `phase0 = true` (asserted today) or `false` (the target, skipped by name with a note)" | TRUE | file has both kinds and the skip-with-note behaviour is in `bench.rs`. |
| C14 | Rule 5: "`arreo record` refuses to write a fixture that looks like a secret without `--allow-secrets`" | TRUE | `cmd_record` (`--allow-secrets`) + `scan_secrets` in `arreo-core::fixtures`. |
| C15 | Rule 5: "`cargo xtask release-check` adds a full-history scan (`gitleaks`, pinned)" | TRUE | `GITLEAKS_PINNED = "8.28.0"`; `ci.yml` checks out with `fetch-depth: 0`. |
| C16 | Rule 6: "`cargo xtask e2e --slice compat` proves it in both directions … There is no live-update handoff yet" | TRUE | `compat_slice`; no update/handoff code. |
| C17 | Pull-request bullets (one approval, two for `arreo-relay*`, DCO sign-off, no CLA) | UNVERIFIABLE | Hosting-side policy; no CLA file exists (consistent), no branch protection is observable anonymously. |
| C18 | Adapter schema example, incl. "defaults are 2000/2000/2500 ms, bell and `done_on_exit` true" | TRUE | `adapters/default.toml` sets exactly those five values. |
| C19 | "`adapters/default.toml` is the universal tier and the one the daemon runs today" | TRUE | `Engine::new(Adapter::default(), …)` in `arreo-server/src/daemon.rs`; `Adapter::default()` = `include_str!("../../../../adapters/default.toml")`. |
| C20 | "`cargo xtask adapters --check` … lints every `adapters/*.toml` and replays each adapter's fixtures, asserting the end state" | TRUE | `xtask/src/adapters_check.rs`. |
| C21 | "The fixture list is a `match` in `xtask/src/adapters_check.rs` (`adapter_fixtures`), one arm per harness" | TRUE | `fn adapter_fixtures(name: &str)` at line 25. |
| C22 | "Per-harness adapter *selection* is not wired yet: the daemon runs the embedded default adapter for every pane" | TRUE | see C19/C21; no selection path in the daemon. |
| C23 | Theme section: copy a built-in from `crates/arreo-core/themes/`, five embedded, loader reads `$XDG_CONFIG_HOME/arreo/themes` and `./.arreo/themes`, "There is no theme gallery to submit to yet" | TRUE | matches `theme::loader` (and `default_dirs()` also honours `$ARREO_THEME_DIR`). |
| C24 | "`arreo-plugin-api` is an empty shell that fixes the crate name and the license boundary, and no code loads a plugin" | TRUE | `crates/arreo-plugin-api/src/lib.rs` = 2 lines, no deps. |
| C25 | License-boundary section: relay is AGPL-3.0-or-later, may depend on Apache first-party crates, nothing Apache may depend on it, no `arreo relay serve` shim | TRUE | license-check.txt §2–§3; no `relay` verb in the CLI dispatch. |
| C26 | Security section: report URL is `…/security/advisories/new` but the setting is **disabled on this repository as of 2026-09-12** | TRUE | GitHub API `private-vulnerability-reporting` → `{"enabled": false}` (checked this session). |
| C27 | "`security@arreo.dev` … and `arreo.dev/security` do not exist pre-launch — the domain has no DNS record" | TRUE | DNS NXDOMAIN. |
| C28 | "The disclosure window is 90 days from the report, matching SECURITY.md" | TRUE | SECURITY.md "Coordinated disclosure on a 90-day window". |
| C29 | "GitHub Discussions … is the only discussion surface that exists today" + link to `/discussions` | **FALSE at snapshot → FIXED during this audit** | API `has_discussions: false`; `https://github.com/ivan-cavero/Arreo/discussions` → **404**. A sibling has since changed the line to the issue tracker with "GitHub Discussions is **not enabled** on this repository" (verified re-read). If the planner wants Discussions enabled, that is a human-gate item. |
| C30 | Code-of-conduct contact bullet: address is pre-launch, "raise it in an issue until launch" | TRUE | `has_issues: true`; DNS NXDOMAIN. |

## 4. SECURITY.md (new in this batch; not yet on `main` — `raw` → 404)

| # | Claim | Verdict | Evidence / required change |
| --- | --- | --- | --- |
| S1 | "There are no releases, no installers, no `arreo.dev`, and no email address at that domain" | TRUE | APIs/DNS as above. |
| S2 | "as of 2026-09-12 it is **disabled** on this repository — GitHub's API reports `{"enabled": false}` for `private-vulnerability-reporting`" | TRUE | verified live: `GET /repos/ivan-cavero/Arreo/private-vulnerability-reporting` → `{"enabled": false}`. |
| S3 | "the 'Report a vulnerability' button does not exist yet" | TRUE | follows from S2. |
| S4 | "The repository owner (`@ivan-cavero`, the only maintainer today)" | UNVERIFIABLE | the owner is `ivan-cavero`; "only maintainer" is a self-statement (no collaborators are visible anonymously). |
| S5 | "`https://github.com/ivan-cavero/Arreo/security/advisories/new` … is this repository's URL today (its `origin` remote)" | TRUE | `git remote -v` matches exactly. |
| S6 | "the launch handoff in `tasks/T-0048-oss-launch-readiness.md` includes creating the project org and moving the repository" | TRUE (wording nit) | the task file's human gate covers visibility/org/announcement and says the loop never does it; phrase it as "the human launch handoff" so nobody looks for an automated step. |
| S7 | In-scope table paths: `arreo pair`, `crates/arreo-core/src/pairing`, `crates/arreo-relay/src/pairing.rs`, `crates/arreo-core/src/identity`, `crates/arreo-server/src/devices.rs`, `crates/arreo-relay`, ADR 0009/0010/0011/0013/0014/0019 | TRUE | all exist (`crates/arreo-server/src/devices.rs` present; ADRs 0009–0019 present). |
| S8 | "No external audit has been performed" | TRUE | no audit artifact in-repo; ROADMAP §4/§6 place it at Phase 5 / pre-revenue. |
| S9 | "Windows Job Objects and macOS rlimits are not implemented (`EnforceError::Unimplemented`)" | TRUE | `crates/arreo-core/src/enforce/other.rs` returns exactly that. |
| S10 | "The Landlock/seccomp profile … does not exist in the code" | TRUE | no `landlock`/`seccomp` occurrence in `crates/`. |
| S11 | "`docs/audit.md` §1 states plainly that the log is append-only by API but not tamper-evident" | TRUE | audit.md §1 first bullet. |
| S12 | "This repo's rule is that no production change lands without a test that fails without it" | TRUE | CONTRIBUTING rule 1; PROMPT/AGENTS protocol. |
| S13 | Safe-harbor paragraphs | UNVERIFIABLE | legal posture, not a repo fact; nothing to check (and nothing contradicts it). |

## 5. docs/** — local (identical to `main` except tour.md, which is being edited now)

### docs/audit.md (581 lines, `5d6d6a06…`; **live on `main`**)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D1 | Header: "(machine schema v7, relay schema v5)" | TRUE | `arreo-core::store::SCHEMA_VERSION = 7`; `arreo-relay::store::SCHEMA_VERSION = 5`. |
| D2 | §2: "`meta.schema_version` is **6** for this build." | **FALSE at snapshot → FIXED during this audit** | code says **7**; a sibling edited the line to "**7**" while this audit was running (verified re-read 02:45Z). Kept here so the defect class is on record. |
| D3 | §2 column tables (v3 `kind`, v4 `action`, v5 `outcome`/`peer`/`detail`) | TRUE | `store.rs` migrations. |
| D4 | §3 "the writers, the CLI's filters and this table all name the same events" | **FALSE at snapshot → FIXED during this audit** | `enforce.alert` (daemon `daemon.rs:231`, graded warn/critical, device `daemon`) was absent from §3's table; a sibling has since added it plus the three `trust.*` rows (verified re-read: `enforce.alert`, `trust.grant`, `trust.revoke`, `trust.refuse` all present with their `detail` shapes). Kept here so the defect class is on record. |
| D5 | §3 audited verbs / "reads and listings leave no row" / `kill`,`resize` unaudited | TRUE | action constants vs writers; §3 and §10 agree. |
| D6 | §4 redaction rules, pattern table, `[REDACTED:*]` behaviour, peer `/24`–`/48` truncation | TRUE | `scan_secrets`/`redact`/`truncate_peer` in `arreo-core::store`. |
| D7 | §5/§6 CLI surface (`audit [--limit] [--json]`, export flags, `prune --before MS`) | TRUE | `cmd_audit` usage lines match. |
| D8 | §7 100 MiB boot warning, no automatic pruning, prune writes its own row | TRUE | `crates/arreo-server/src/audit.rs::AUDIT_WARN_BYTES = 100*1024*1024`. |
| D9 | §11 relay trail (actions, `arreo-relay audit export/prune --state-dir`, 100 MB warning) | TRUE | `crates/arreo-relay/src/audit.rs` constants; `main.rs::audit`; `AUDIT_WARN_BYTES` in `router.rs`. |

### docs/relay-deploy.md (860 lines, `1b3c3e01…`; **live on `main`**)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D10 | §1/§2/§5/§9 flags, defaults (`--listen` default `127.0.0.1:8787`, `--state-dir` required, re-keying replaces the root key, license split) | TRUE | `DEFAULT_LISTEN`; flag parsing in `main.rs`; §9.0 matches LICENSE-RELAY. |
| D11 | §3 "`meta.schema_version` is **3** for this build" | **FALSE at snapshot → FIXED during this audit** | code: **5**; a sibling has since corrected it to "**5**" (verified re-read). |
| D12 | §3 table list (`meta`, `account`, `relay_device`, `machine`, `inbox`, `inbox_stats`) | **FALSE at snapshot → FIXED during this audit** | v5 adds `relay_audit` (T-0053) and v4 adds `machine.daemon_key` (T-0045) — both missing from the list; a sibling has since added both rows with the right column sets (verified re-read). |
| D13 | §3 "the raw material for presence (T-0031), **which does not exist yet**" | **FALSE at snapshot → FIXED during this audit** | `crates/arreo-relay/src/presence.rs` ships; `router.rs` derives presence for directory rows; a sibling has since inverted the sentence (verified re-read). |
| D14 | §8.4 "presence does not exist yet (T-0031), so there is no 'last seen 2 days ago' answer" | **FALSE at snapshot → FIXED during this audit** | same; corrected in place (verified re-read). |
| D15 | §13 "No presence yet … Presence is T-0031." | **FALSE at snapshot → FIXED during this audit** | same; the section now says "Presence is derived at read time" (verified re-read). |
| D16 | §13 "Refusals live only in stderr. There is no durable audit table to query yet (T-0033)" | **FALSE at snapshot → FIXED during this audit** | `relay_audit` ships and `arreo-relay audit` reads it (docs/audit.md §11); a sibling has since corrected the section (verified re-read). |
| D17 | §6 handshake budget 3/10 s, cleared on success; §8 inbox bounds 30 d/10 000/64 MiB with hourly sweep; §13 no push, at-least-once, no global cap | TRUE | `HANDSHAKE_MAX_ATTEMPTS=3`/`HANDSHAKE_WINDOW=10s`, `DEFAULT_TTL_DAYS=30`, `DEFAULT_MAX_MESSAGES=10_000`, `DEFAULT_MAX_MB=64`; no push in `crates/`. |
| D18 | §12 the daemon's relay leg (outbound dial, probe = pane count, role-gated verbs, pinned peer) | TRUE | `crates/arreo-server/src/relay_client.rs`; §12.7's gaps match the code. |

### docs/relay-protocol.md (783 lines, `6e769a82…`; **live on `main`**)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D19 | §1–§4 handshake/envelope/kind wire contract; v1 everywhere | TRUE | `RELAY_VERSION = 1`; `RelayKind` names in `arreo-core::relay::mod`. |
| D20 | §6 caps (1 MiB envelope, 16 KiB handshake, 64-item outbound queue, 256 per drain, 3/10 s budget, 10 s QUIC timeout, 2 s refusal grace) | TRUE | `MAX_ENVELOPE_BYTES`, `MAX_HANDSHAKE_BYTES`, `OUTBOUND_QUEUE=64`, `DEFAULT_DRAIN_LIMIT=256`, `CONNECT_TIMEOUT=10s`, the 2 s `connection.closed()` timeout. |
| D21 | §5.1 "(Durable audit rows are T-0033; today stderr is the whole record.)" | **FALSE at snapshot → FIXED during this audit** | relay audit ships (D16); a sibling has since corrected the parenthetical (verified re-read: the refusal is "also logged … and written as rows in the relay's own `relay_audit` table"). |
| D22 | §8.3 "**No durable audit rows (T-0033).** Refusals are written to stderr." | **FALSE at snapshot → FIXED during this audit** | same; the section now says "Refusals are durable, in the relay's own table (T-0053)" (verified re-read). |
| D23 | §8.3 "**No presence (T-0031).** … nothing derives online/offline from it yet" | **FALSE at snapshot → FIXED during this audit** | same as D13; the section now says "Presence is derived at read time, not pushed (T-0031)" (verified re-read). |
| D24 | §8.3 "No revocation list (T-0026)" | TRUE | no revocation code in `crates/arreo-relay`. |
| D25 | §8.3 "The relay does not encrypt payloads … a client that sends plaintext gives the relay plaintext" | TRUE | honest limit; matches `README` §"How the relay fits". |
| D26 | §4.5 at-least-once + consumer-side `(src_device, seq)` de-duplication | TRUE | inbox rows deleted only by `ack`; `inbox.rs` / `router.rs`. |

### docs/machines.md (256 lines, `15f2d17b…`; **live on `main`**)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D27 | Verbs (`list/status/rename/remove/add/trust`), exit codes 0/2/3/4/5, `--json` schema 1 additive-only, "the human table is not a contract" | TRUE | `machines.rs` (`SCHEMA: u32 = 1`; usage prints the exit-code legend; the table disclaimer is in the usage text). |
| D28 | Cache at `identity/machines.cache`, config via `--config`/`$ARREO_CONFIG`, no default path, relay read is direct (no daemon) | TRUE | `arreo_core::mesh::resolve` reads `$ARREO_CONFIG`; `docs/tour.md` "Where state lives" agrees. |
| D29 | `presence` closed enum `online/offline/stale/unknown`; `machine_id` bare 32 hex | TRUE | `mesh::directory::Presence` + parse/`presence_of`; directory test locks the enum. |
| D30 | Trust model: grants are local, `--machine` must name this machine, `trust` refuses unpinned devices, audit rows `trust.grant/revoke/refuse` | TRUE | `machines.rs::trust`; action constants `TRUST_GRANT/TRUST_REVOKE/TRUST_REFUSE` in `store.rs`; `daemon.rs`. |

### docs/cross-os.md (52), docs/conpty-windows.md (36), docs/release.md (17), docs/agent-skill.md (51)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D31 | cross-os §layers: layer 1 `check-targets` exists and proves cfg-cleanliness only; layer 2 is the CI matrix; layer 3 (xwin/osxcross) is deferred | TRUE (layer 2 unproven) | `xtask/src/check_targets.rs`; `ci.yml` matrix exists, but read D32. |
| D32 | cross-os: "**Real build + full tests + conpty-smoke on real OSes** … the OS matrix job is the authority for skipped targets" | **FALSE at snapshot → FIXED during this audit** | the matrix had never been green (90/90 failures), so nothing had been proven on macOS or Windows. A sibling has since stated the red state in place (verified re-read: layer 2 "when it is green. It is not green today" + the "currently red" paragraph). CI itself still needs fixing. |
| D33 | cross-os: the planted `std::os::unix` import failed the gate with `error[E0433]`, evidence in `.loop/evidence/T-0010/` | TRUE | `gate-negative.txt` line 5 contains the exact `error[E0433]: cannot find `unix` in `os``. |
| D34 | conpty-windows: real ConPTY on Windows, same `Pane` mechanics on unix, conhost degradation, resize/kill mappings, "no Wine claims" | TRUE as scoped | `xtask conpty-smoke`; the doc states its own limits and defers the ConPTY proof to the (currently red) Windows runner. |
| D35 | release.md: reserved install URLs, unsigned artifacts until signing lands, `package --dry-run` on PRs, pre-revenue audit gate | TRUE (marked pre-launch in place) | the file states RESERVED/UNSIGNED; `package --dry-run` is a CI step. |
| D36 | release.md: "`brew install arreo/tap/arreo`" | **FALSE at snapshot → FIXED during this audit** | it disagreed with `Cargo.toml`'s dist `tap = "arreo/homebrew-arreo"`; a sibling has since annotated the row in place ("The `arreo/homebrew-arreo` tap … does not exist"). Re-checked 02:45Z. |
| D37 | agent-skill: the verb list, states, durations, and "Every example above was executed against the real daemon this turn (see `.loop/evidence/T-0014/`)" | TRUE | verbs verified individually; `T-0014/verify.log` records the api tests plus "skill doc executed verbatim". |

### docs/tour.md (being written by a sibling during this audit)

| # | Claim | Verdict | Evidence |
| --- | --- | --- | --- |
| D38 | pre-launch status paragraph ("no releases, no published crates, no install script and no `arreo.dev`") | TRUE | as §0. |
| D39 | "Five themes are embedded", `cargo xtask` command table incl. `bench --probe proto|size`, `release-check`, TUI keys `j/k/Enter/w/t//q`, "Phones, the web client, config sync, auto-update, plugins, push notifications and the approvals gates are **not built**" | TRUE | `BUILTINS` (5); `bench.rs` usage names both probes; `crates/arreo-tui/src/ui.rs::on_key` + the TUI status line; absence checks as R27. |

## 6. NOTICE.md — before/after my edit (§5 of license-check.txt has the full detail)

| # | Claim | Verdict |
| --- | --- | --- |
| N1 | two-license split, crate lists | TRUE after edit (was TRUE for the crates it named, with three wrong/invented entries added on) |
| N2 | third-party table rows (alacritty_terminal Apache-2.0; portable-pty MIT; ratatui/crossterm MIT; quinn MIT OR Apache-2.0; rusqlite MIT; tokio MIT) | TRUE — each crate is in `Cargo.lock` and each license matches its own manifest in the registry |
| N3 | "uniffi (MPL-2.0, Phase 3) and wasmtime (Apache-2.0 WITH LLVM-exception, Phase 4) are **not** in this tree today" | TRUE (`0` matches in `Cargo.lock` for both) |
| N4 | "Licenses are gated in CI (`cargo deny check`, configured by `deny.toml`)" | TRUE (CI step + `deny.toml [licenses]`) — note this gate cannot pass while CI is red |
| N5 | "There are no releases yet and no `arreo.dev`" | TRUE |
| N6 | trademark paragraph (Apache-2.0 §6 grants no trademark rights → forks must rename; boundary in CONTRIBUTING + ROADMAP §7) | TRUE (LICENSE §6 read in full; both pointers resolve) |
| N7 | (removed) rust-noise row | was FALSE — `snow` is the dependency |
| N8 | (removed) "ships with every release at `https://arreo.dev/notices/<version>`" | was FALSE — no releases, no domain, no SBOM artifact |
| N9 | (removed) `docs/forking.md` | was FALSE — file does not exist (and was a code span, so `xtask`'s links gate never saw it) |
| N10 | (removed) rusqlite "MIT / blessing" | was misleading — reworded to "MIT (the bundled SQLite is public domain)" |
| N11 | (removed) "the mobile and web clients" as licensed present software | was FALSE — reworded as an explicit plan statement |

## 7. Outside the named file set (reported, not edited)

| # | Claim | Verdict | Where |
| --- | --- | --- | --- |
| O1 | "Report to **<conduct@arreo.dev>**" with no pre-launch note | FALSE today (NXDOMAIN) | `CODE_OF_CONDUCT.md:33` — one-line fix mirroring CONTRIBUTING's C1 |
| O2 | `design/landing.html` ships `curl -fsSL https://arreo.dev/install.sh \| sh`, `href="#"` buttons, "macOS · Linux · Windows" meta line | FALSE if it becomes the page | `design/landing.html:214,217,357` — it is a design mock today; if it is ever published it inherits every README defect |
| O3 | `design/BRAND.md` "Web dashboard (app.arreo.dev …)" | OK as design intent (labeled a design doc) | `design/BRAND.md:94` |
| O4 | `ROADMAP.md` §3.10 "Baseline across browsers since March 2026 (Safari 26.4 closed the last gap)" and the §3 research claims | UNVERIFIABLE (external facts) | roadmap register; it is a plan document and is labelled as one — no change required for launch, but it is the one place with externally-checkable factual claims that nothing in this repo can settle |

## 8. What must change, file by file (for the planner to route)

Ordered by risk. **I own only NOTICE.md**; everything else needs its owner.

**Blocking before this is called launch-ready**

1. **CI green on all three OSes** (`.github/workflows/ci.yml`, product code). Current HEAD
   `4c249cc` is red: ubuntu `cargo test --workspace` (exit 101 — a panicking test),
   windows `cargo build` (exit 1), macos `clippy -D warnings` (exit 1). Nothing else on
   this list is as expensive, because the repository is already public. Run
   `https://github.com/ivan-cavero/Arreo/actions/runs/34666329483` for the evidence.
   (The *sentences about* CI — R13, R35, C6, C12, D32 — were fixed in place by a sibling
   during this audit: they now state the red status. The redness itself is still open.)
2. **Push the rewritten `README.md`, `CONTRIBUTING.md`, `NOTICE.md`, `SECURITY.md`** —
   `main` currently serves the old versions (§2, L1–L23). Until they land, the false claims
   are the public face.
3. ~~`README.md` R13/R35/R40~~ — fixed in place by a sibling (verified re-read).
4. ~~`CONTRIBUTING.md` C6/C12/C29~~ — fixed in place by a sibling (verified re-read);
   C4 ("see §8") was already correct as an anchor link.
5. ~~`docs/relay-deploy.md` D11–D16~~ — fixed in place by a sibling (verified re-read).
6. ~~`docs/relay-protocol.md` D21–D23~~ — fixed in place by a sibling (verified re-read).
7. ~~`docs/audit.md` D2/D4~~ — fixed in place by a sibling (verified re-read).
8. ~~`docs/cross-os.md` D32~~ — sentence fixed in place; the underlying CI-red cause is
   item 1 above.
9. ~~`docs/release.md` D36~~ — fixed in place by a sibling (verified re-read).
10. **`CODE_OF_CONDUCT.md:33`:** add the pre-launch note CONTRIBUTING already carries (O1).
    Still open.

**Also worth doing in the same pass**

11. `REUSE.toml`: drop or mark `mobile/**`, `web/**`, `themes/**` (the paths do not exist);
    `themes/` in particular is a *wrong* path — the theme files are at
    `crates/arreo-core/themes/*.json`; consider `adapters/**` plus the currently-uncovered
    `tasks/**`, `specs/**`, `fixtures/**`, `xtask/**` (and whatever should be ignored, e.g.
    `.loop/**`) so the pinned `reuse lint` can pass. `LICENSE-RELAY` is named twice, which
    aggregates to `Apache-2.0 AND AGPL-3.0-or-later` for a pure-AGPL file — put it in one
    annotation. See license-check.txt §4.
12. `xtask/tests/workspace_deps.rs`: the REUSE existence check skips continuation lines of a
    multi-line `path = [...]` array, which is why the three dead patterns pass today — parse
    the array to its closing bracket, or defer to `reuse lint`. Also consider teaching the
    `release-check` links item about code-span paths, which is how `docs/forking.md` hid.
13. `design/landing.html`: if it ships, replace the reserved install URL and the `href="#"`
    links (O2).

## 9. Verdict summary

| File / set | Claims | TRUE | FALSE | UNVERIFIABLE | FALSE rows |
| --- | --- | --- | --- | --- | --- |
| README.md (working tree) | 40 | 39 | 0 | 1 | fixed mid-audit: R13, R35, R40 (UNVERIFIABLE: R2) |
| CONTRIBUTING.md (working tree) | 30 | 29 | 0 | 1 | fixed mid-audit: C6, C12, C29 (UNVERIFIABLE: C17) |
| SECURITY.md | 13 | 11 | 0 | 2 | — (UNVERIFIABLE: S4, S13) |
| docs/** | 39 | 38 | 1 | 0 | D32's cause (CI red) is the one open defect; D2, D4, D11–D16, D21–D23, D36 fixed mid-audit |
| NOTICE.md (after my edit) | 11 | 11 | 0 | 0 | — (N7–N11 were FALSE before the edit) |
| Outside the named set | 4 | 1 | 1 | 2 | O1 (UNVERIFIABLE: O2 conditional, O4) |
| **Total** | **137** | **129** | **2** | **6** | |
| *live on `main` right now (set view of the same files, pre-batch)* | *23* | *0* | *23* | *0* | *L1–L23* |

The live-on-`main` rows are the pre-batch text of files already counted above, so they are
**not** added to the total: they measure the same defects against what strangers read
today, and they disappear when this batch is pushed.

## 9b. Post-snapshot re-check (siblings are editing the same tree)

Re-read at 02:45Z, after the tables above were written:

- `docs/audit.md` → `c7070014…`: §2's schema number is now **7** (D2 fixed). §3 still has
  no `enforce.alert` row (D4 open).
- `docs/release.md` → `07fde4ad…`: the Homebrew row now says the `arreo/homebrew-arreo`
  tap does not exist (D36 fixed).
- `docs/relay-deploy.md` `1b3c3e01…`, `docs/relay-protocol.md` `6e769a82…`: **unchanged**,
  so D11–D16 and D21–D23 are still open exactly as written.
- `CONTRIBUTING.md` `ec02808e…`, `README.md` `4b31abda…`, `SECURITY.md` `ec2ff2de…`,
  `NOTICE.md` (mine): unchanged.
- `docs/tour.md` is genuinely in flight (its hash moved twice during the audit); the
  tour.md rows are judged against the version I could read, and it should be re-audited
  once its author stops writing.

Nothing else in the tree moved. The planner should re-run this audit's grep-able checks
(the four stale-claim greps and the CI run query) after the batch settles — the two
mid-audit fixes show how fast these verdicts can change.

## 10. Anything I could not check

- CI **logs** need authentication (403 anonymously); the failing step names and exit codes
  come from the public jobs API and the check-run annotations.
- `crates.io`'s HTML pages 404 to `curl` while the API answers 200 — a fetch artifact, not
  a dead link; do not "fix" those URLs on that signal.
- `cargo test`, `cargo build`, clippy and fmt were **not** run (shared checkout, T-0048
  rule); every test-shaped claim here is "read from the source", and `xtask release-check`
  was not executed for the same reason. The planner's final gate run is the authority.
- `reuse lint` could not run (`reuse` not installed, no `pip` here) — the coverage verdict
  in license-check.txt is a hand-check of path coverage only.
- Files hashed for this audit (so a later reader can tell whether the text moved):
  README.md `4b31abda…`(300 ln), CONTRIBUTING.md `ec02808e…`(237), NOTICE.md
  `1888287a…`(51, mine), SECURITY.md `ec2ff2de…`(143), docs/audit.md `5d6d6a06…`(581),
  docs/relay-deploy.md `1b3c3e01…`(860), docs/relay-protocol.md `6e769a82…`(783),
  docs/machines.md `15f2d17b…`(256), docs/cross-os.md `a7f54fbd…`(52),
  docs/conpty-windows.md `505cbdb7…`(36), docs/release.md `6d4b6baf…`(17),
  docs/agent-skill.md `5fe88e89…`(51), docs/tour.md `afc32d8f…`(197, sibling in flight).
