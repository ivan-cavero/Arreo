# T-0048 · repo furniture (issue forms, PR template, SECURITY.md)

Owner: RepoFurniture. Scope: `.github/ISSUE_TEMPLATE/{bug,feature,adapter-request}.yml`,
`.github/PULL_REQUEST_TEMPLATE.md`, `SECURITY.md`, this file. Nothing else was edited.

## Files written, and why each exists

| File | One-line reason |
| --- | --- |
| `.github/ISSUE_TEMPLATE/bug.yml` | A bug report is only actionable if it carries the build (`arreo --version` + commit), the OS/terminal, the exact commands, expected vs actual, and the daemon log / `arreo audit --json` — so the form asks for exactly those and refuses to file a security report. |
| `.github/ISSUE_TEMPLATE/feature.yml` | Forces the request to be a problem with a proposal and considered alternatives, because this repo plans work by phase in `tasks/` and a patch-shaped request cannot be scheduled. |
| `.github/ISSUE_TEMPLATE/adapter-request.yml` | Adapters are TOML data tested against recorded PTY sessions, so the useful unit of work is "harness + version + verbatim prompt bytes + can you provide a fixture", not a feature request. |
| `.github/PULL_REQUEST_TEMPLATE.md` | Makes a PR state its evidence (named e2e slice, OSes), the three mechanical bars (`cargo test --workspace`, clippy zero warnings, fmt clean), the TDD bar, the no-secrets bar, the ADR question and the DCO/relay-approval rules. |
| `SECURITY.md` | CONTRIBUTING §8's disclosure process, with the scope drawn from ROADMAP §4 and the code, and the honest state of the private channel: it does not exist until one repository setting is flipped. |
| `.loop/evidence/T-0048/furniture.md` | This record. |

## Verification performed (exact checks)

### 1. YAML parses and conforms to GitHub's issue-form schema

Validated with the system `python3` + PyYAML 6.0.2 (`yaml.safe_load`), **outside the
workspace** — no dependency added to the repo. Assertions, beyond "it parses":

- top-level keys ⊆ {`name`, `description`, `title`, `labels`, `assignees`, `body`, `projects`};
- every block's `type` ∈ {`markdown`, `textarea`, `input`, `dropdown`, `checkboxes`};
- non-markdown blocks have a unique `id` and `attributes.label`; markdown blocks have
  `attributes.value`; dropdowns have non-empty `options`; checkbox options have `label`;
- `validations` contains only `required`; every `id` ≤ 20 characters.

Result: `bug.yml` — 11 blocks (markdown, input, input, dropdown, input, dropdown,
textarea ×4, checkboxes); `feature.yml` — 6 blocks; `adapter-request.yml` — 8 blocks.
**Errors: none.**

### 2. Labels exist (verification, not invention)

```console
$ curl -s https://api.github.com/repos/ivan-cavero/Arreo/labels | jq -r '.[].name'
accessibility  bug  documentation  duplicate  enhancement  good first issue
help wanted  invalid  question  wontfix
```

`bug.yml` uses `bug`; `feature.yml` and `adapter-request.yml` use `enhancement`. No
other label is named anywhere in the four files. `.github/` contains no label
definition (only `workflows/`), so the API above is the only ground truth; both labels
used are also GitHub per-repo defaults, so they survive the launch move to a new org
repository. `GET /repos/ivan-cavero/Arreo` confirms this checkout's `origin`
(`full_name: ivan-cavero/Arreo`, `visibility: public`, `owner: ivan-cavero`).

### 3. Every path and command named exists in this repo

Extracted every backticked token from the four `.github` files + `SECURITY.md` and
tested path-like tokens with `pathlib.Path.exists()`:

- resolved: `adapters/{default,opencode,pi}.toml`, `adapters/{opencode,pi}-native.md`,
  `docs/agent-skill.md`, `docs/audit.md`, `crates/arreo-core/src/{fixtures.rs,store.rs}`,
  `crates/arreo-core/src/{identity,pairing,transport,enforce}`, `crates/arreo-server`,
  `crates/arreo-server/src/devices.rs`, `crates/arreo-relay`, `crates/arreo-relay/src/pairing.rs`,
  `xtask/src/adapters_check.rs`, `specs/adr/`, `tasks/`, `docs/`,
  `tasks/T-0019-resource-enforcement.md`, `tasks/T-0048-oss-launch-readiness.md`;
- intentional non-file tokens: `adapters/*.toml`, `fixtures/*.pty`,
  `fixtures/<harness>-<scenario>.pty` (globs/hints; `fixtures/` holds 13 `.pty` files),
  `crates/arreo-relay*` (a crate glob), `/security/advisories/new` (URL fragment);
- not present on this box because it is created by the installer:
  `~/.config/systemd/user/arreo.service` — comes from `arreo-core/src/lifecycle.rs:100-104`
  (`unit_path(ServiceKind::SystemdUser)`), and `arreo service install` writes it.

Commands named, each traced to its source of truth:

| Named | Truth |
| --- | --- |
| `arreo --version` | `crates/arreo-cli/src/main.rs:114` |
| `arreo spawn/attach/read/send/wait/panes/split` | usage block, `main.rs:39-90` |
| `arreo wait … --timeout 10s` | `--timeout`, durations `500ms/30s/5m` (`docs/agent-skill.md`) |
| `arreo service install` | `main.rs:768` (`cmd_service`); unit path from `lifecycle.rs` |
| `arreo audit --json`, `arreo audit export` | usage, `main.rs:56-63` |
| `arreo audit --json` content claim (redaction) | `store.rs::redact` writes `[REDACTED:<label>]` and `redacted=1` |
| `arreo record <cmd> -o <path> [--allow-secrets]` | `main.rs:206-270` |
| `arreo devices`, `arreo pair` | usage; `main.rs:141-142` |
| `journalctl --user -u arreo` | unit is `arreo.service` → `-u arreo` |
| `cargo run -p arreo-server -- --socket PATH` | `arreo-server/src/main.rs:19-30` |
| `cargo test --workspace`, `cargo clippy … -D warnings`, `cargo fmt … --check` | `AGENTS.md` §Commands; CI steps |
| `cargo xtask bench`, `cargo xtask adapters --check` | `xtask/src/main.rs:40,44` |
| ten e2e slices in the PR template | `xtask/src/main.rs:59-71`: chaos, api, compat, state, lifecycle, enforcement, persistence, tui, theme, relay |

Negative finding encoded in the PR template: `cargo xtask e2e` with **no** `--slice`
prints `xtask e2e: not implemented` and exits 0 (`xtask/src/main.rs:74-83`), so the
template says in as many words that it is not evidence.

### 4. SECURITY.md's channel is real today — and it is not

```console
$ curl -s https://api.github.com/repos/ivan-cavero/Arreo/private-vulnerability-reporting
{"enabled": false}
$ getent hosts arreo.dev          # no output → no DNS record
$ curl -o /dev/null -w '%{http_code}' https://github.com/ivan-cavero/Arreo        # 200
$ curl -o /dev/null -w '%{http_code}' https://github.com/arreo-dev/arreo          # 404
```

So: the only candidate private channel (GitHub private vulnerability reporting) is
**disabled**, and the address CONTRIBUTING §8 currently prints (`security@arreo.dev`)
cannot receive mail because the domain does not resolve. `SECURITY.md` therefore says
in its second paragraph that the file is incomplete until the setting is flipped, names
the setting (**Settings → Security and quality → Advanced Security → Private
vulnerability reporting → Enable**) and the admin API equivalent, and states that
GitHub's private vulnerability reporting is the mechanism once enabled — the report URL
is the repository's `/security/advisories/new`. The API semantics were checked against
GitHub's REST reference (`PUT /repos/{owner}/{repo}/private-vulnerability-reporting`,
204, admin required; the GET returns `{"enabled"}`).

Scope claims were checked against code, not the roadmap alone: pairing SPAKE2 with a
300 s default TTL (`arreo-core/src/pairing/flow.rs:64`, ADR 0010), identity
(`arreo-core/src/identity`, ADRs 0009/0019), Noise-KK inside QUIC
(`arreo-core/src/transport`, ADR 0011), relay auth/routing (`crates/arreo-relay`,
ADRs 0013/0014), audit redaction (`store.rs::redact`), fixture scanning
(`fixtures.rs::scan_secrets`), Linux-only cgroup v2 guard with
`EnforceError::Unimplemented` elsewhere (`arreo-core/src/enforce/mod.rs:37`).
`grep -rn 'landlock\|seccomp' crates/ docs/ specs/ tasks/` matches only the ROADMAP §4
table row — there is no implementation — so Landlock/seccomp is listed as **out of
scope**, not as a defended boundary. Audit posture: no external audit has happened;
ROADMAP §4 makes it the pre-revenue gate and §6 puts it in Phase 5 — stated as a
commitment, and readers are asked to report any "audited" wording as a defect.

### 5. No placeholder text

`grep -rniE 'todo|fixme|coming soon|tbd|owner/repo|xxx|lorem|<insert'` over the five
files → no matches. The only `placeholder:` keys are GitHub form hints, each carrying
either a literal instruction ("the line your build prints") or a real command block
(`cargo run -p arreo-server -- --socket /tmp/arreo.sock`); no invented versions or
invented URLs remain in them.

## Could not verify — exact questions

1. **Which repository the public launch will live in.** I used
   `https://github.com/ivan-cavero/Arreo/...` because it is the only URL that answers
   today (200; this checkout's `origin`); `https://github.com/arreo-dev/arreo` (the URL
   README uses) is 404 and the `arreo-dev` org is 404. **Question:** if the launch
   creates the org and moves the repo, is updating `SECURITY.md`'s two URLs part of the
   T-0048 human-gate checklist? Labels are safe either way (per-repo defaults include
   `bug`/`enhancement`); the URLs are not.
2. **Whether the "Report a vulnerability" button 404s or is hidden while PVR is
   disabled.** I did not create a test advisory to find out; the API's
   `{"enabled": false}` is the fact I relied on. **Question for the human gate:** after
   flipping the setting, confirm the button is visible on the repository's Security tab
   before announcing the launch — that is the one thing this file asserts that only a
   logged-in browser can check.
3. **Whether `.github/ISSUE_TEMPLATE/config.yml` should exist** (blank-issue link,
   discussion link, security link). I did not create it: it is not in my file list and
   its `contact_links` would want a real support URL, which does not exist pre-launch.

## Defects found outside my ownership (reported, not edited)

- `CONTRIBUTING.md`: `cargo xtask setup` is not a registered xtask verb (registered:
  `e2e`, `bench`, `demo`, `conpty-smoke`, `check-targets`, `adapters`, `package`,
  `release-check`); `cargo xtask e2e` is described as "the definition of works" but is
  still the stub at `xtask/src/main.rs:74`; the adapter example uses `match.process`,
  which is not a field of the adapter schema (`arreo-core/src/state/adapter.rs:55-65`:
  `idle_after_ms`, `question_after_ms`, `blocked_after_ms`, `bell_means_attention`,
  `done_on_exit`, `question_patterns`, `error_patterns`) and would fail
  `cargo xtask adapters --check`; the fixture path `fixtures/myharness/*.pty` is wrong
  (fixtures are flat files in `fixtures/`, and a new harness must be added to
  `adapter_fixtures()` in `xtask/src/adapters_check.rs`); §8's `security@arreo.dev` and
  `arreo.dev/security` are not reachable.
- `README.md`: quickstart uses `arreo server init` (only `arreo server stop` exists,
  `main.rs:891-896`) and a bare `arreo` to "attach the TUI" (a bare invocation prints
  usage; the TUI binary is `arreo-tui`); the "release", "crates.io" and
  "security: externally audited" badges and the `arreo.dev` install URLs are pre-launch.
- `tasks/T-0048-oss-launch-readiness.md`: the handoff text includes
  `gh repo edit --visibility public`, but the repository is already
  `"visibility": "public"` per the GitHub API today.
