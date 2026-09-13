# Harness matrix — how 18 agent CLIs report state, resume, and take custom models

One sentence: the per-harness facts adapter-suite v2 (new TOMLs), harness-aware
resume (T-0072), and harness config sync (ROADMAP §3.8) have to be built from —
each row naming the exact CLI version it was checked against, and each non-empty
cell traceable to a transcript under `.loop/evidence/T-0075/`.

Seeded by ROADMAP §§3.8–3.9 plus T-0017 (pi + opencode, the two deep adapters).
Discovery sources actually executed on this box: `herdr integration status` (17
harnesses with their hook/plugin entry points), `orca agent-context --json` and
`orca skills list` (the Orca CLI's own harness vocabulary), and the installed
integration scripts those two products ship.

## 1. Provenance labels

Every row carries one label; a cell that is not backed by a transcript says so.

| Label | Meaning |
| --- | --- |
| **`live`** | The CLI was executed on this box inside an isolated `HOME`/`XDG_*`/scratch dir this turn; the transcript is named in the row. |
| **`host`** | The CLI is **not installed**; the cell comes from an integration artifact that a real product (herdr, Orca) installed/wrote on this box, read verbatim. Proves the hook point exists, not the CLI's own behaviour. |
| **`untried`** | Nothing was executed and nothing on this box documents it. The cell is empty on purpose — never filled from memory. |

Version claims use `--version` output verbatim. "Untried — version unknown" is a
complete answer; guessing is not.

## 2. The matrix

| # | Harness | Version checked | State signals (hooks/events, OSC, prompt, bell, latency) | Resume (exact argv + session store) | Custom models / providers (paths, fields, env) | Prov. |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | **pi** | `0.84.4` | `--mode json` NDJSON: `session`→`agent_start`→`turn_start`→`message_start/_update/_end`→`turn_end`→`agent_end`→`agent_settled` (pi-verified). Extension API: `pi.on("session_start"\|"agent_start"\|"agent_settled")` + a `pi.events` bus, with `ctx.sessionManager.getSessionId()/getSessionFile()`, `ctx.mode`, `ctx.isIdle()` (pi-verified, herdr's installed pi extension). TUI emits `OSC 0;π - <cwd> BEL` title, `OSC 8;;` hyperlinks, `OSC 11;?` query — **all BELs are OSC terminators** (60/60 in 10 s idle). No OSC 9/777/133. No alt-screen. Latency: silence thresholds 2000/2000/2500 ms (`adapters/pi.toml`). | `--session-id <uuid>` **pins** (creates if missing); `--session <path\|id-prefix>` resumes; `--continue` = most recent in cwd; `--fork <path\|id>` branches. Store: `<PI_CODING_AGENT_DIR>/sessions/<cwd-slug>/<ts>_<uuid>.jsonl` (flat `<ts>_<uuid>.jsonl` under `--session-dir`). | `<PI_CODING_AGENT_DIR>/models.json` → `providers.<id>.{baseUrl,api,apiKey,authHeader,models[]}`; `apiKey` accepts a literal **or** `$VAR` / `${VAR}` (both verified); `auth.json` holds OAuth; env fallbacks per `pi --help` (`OPENAI_API_KEY`, `XAI_API_KEY`, … 40+). | `live` |
| 2 | **opencode** | `1.18.30` | Plugin API `event`/`chat.message`: `permission.asked`,`question.asked`→blocked; `permission.replied`,`question.replied`,`question.rejected`,`tool.execute.before/after`,`session.compacted`→working; `session.status{idle,active,busy,pending,retry,running,streaming,working}`; `session.idle`; `session.error`. `run --format json` emits `step_start`/`text`/`step_finish` each carrying `sessionID`. TUI: title OSC 0, alt-screen, OSC 99 notification channel (probe only) — BELs are OSC terminators (5/5). Latency: 2000/2000/2500 ms. | `-s/--session <ses_…>` resumes; `-c/--continue` = last session **of this project**; `--fork`. IDs visible only in `--format json` events / `session list --format json`. Store: SQLite `$XDG_DATA_HOME/opencode/opencode.db` (tables `session`,`message`,`part`,`credential`,`account`). | `$XDG_CONFIG_HOME/opencode/opencode.json{,c}` → `provider.<id>.{name,npm,options.{baseURL,apiKey},models.<m>.{id,name,tool_call,interleaved,modalities,limit}}`; `apiKey: "{env:VAR}"` works and is load-bearing; project `<cwd>/opencode.json` merges on top. `opencode providers login` for OAuth. | `live` |
| 3 | **omp** (Oh My Pi) | `omp/18.1.16` | Same NDJSON envelope family as pi (`session`→`agent_start`→`turn_start`→…→`agent_end`) plus `advisor_cost_changed`. Extension hooks used by the two extensions loaded into the live omp on this box (Orca's `orca-agent-status.ts`, herdr's `herdr-omp-agent-state.ts`): `session_start`,`session_switch`,`agent_start`,`before_agent_start`,`message_end`,`tool_call`,`tool_execution_start/end`,`tool_approval_requested/resolved`,`agent_end`,`agent_settled`,`session_shutdown`. No TUI capture taken (not measured). Latency: inherits the universal thresholds; not measured. | `-r/--resume <id-prefix\|path>` and `-c/--continue` both re-open the same file (verified: second run reuses id `01a099e5-…`, no new file). Store: `<agent-dir>/sessions/<cwd-slug>/<ts>_<uuid>.jsonl`, flat under `--session-dir`. `--from-claude` / `--from-codex` import foreign sessions. | `<agent-dir>/models.yml` → `providers.<id>.{name,baseUrl,api,apiKey,models[]}`; `apiKey` accepts a literal **or `$VAR`** — **`${VAR}` does NOT substitute** (verified: literal sent, HTTP 401). `config.yml` holds `modelRoles.default`, theme, composer shape. `omp models [provider]`, `omp token`, `omp auth-broker` (credential vault). | `live` |
| 4 | **Codex** | untried — version unknown | `host`: Orca ships a Codex hook wiring at `~/.config/orca/codex-runtime-home/home/hooks.json` with events **`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `SubagentStart`, `SubagentStop`, `Stop`** (hashed/trusted in the same dir's `config.toml`). CLI untried. | untried. Resumable-session argv not executed. Herdr expects a hook at `~/.config/orca/codex-runtime-home/home/herdr-agent-state.sh` (host path, unverified against codex). | untried (config path not executed). | `host` |
| 5 | **[CC]** | untried — version unknown | `host` only: herdr's integration registry expects its state hook at `~/.claude/hooks/herdr-agent-state.sh`; `~/.claude/` exists on this box but is empty and the CLI is absent. Nothing else was executed. | untried. | untried. | `untried` |
| 6 | **Gemini CLI** | untried — version unknown | untried. Herdr's registry names an integration for `antigravity-cli` (hook at `~/.gemini/config/hooks/herdr-agent-state.sh`) — a *different* CLI from Gemini CLI; do not conflate (see row 18). | untried. | untried. | `untried` |
| 7 | **Grok CLI** | untried — version unknown | untried. Herdr's registry expects `~/.grok/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 8 | **GitHub Copilot CLI** | untried — version unknown | untried. Herdr's registry expects `~/.copilot/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 9 | **Cursor CLI** (`cursor-agent`) | untried — version unknown | untried. Herdr's registry expects `~/.cursor/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 10 | **Devin CLI** | untried — version unknown | untried. Herdr's registry expects `~/.config/devin/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 11 | **Droid** (Factory) | untried — version unknown | untried. Herdr's registry expects `~/.factory/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 12 | **Kimi Code** | untried — version unknown | untried. Herdr's registry expects `~/.kimi-code/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 13 | **Kilo** | untried — version unknown | untried. Herdr's registry expects a **JS plugin** at `~/.config/kilo/plugin/herdr-agent-state.js` (plugin API, not shell hooks). | untried. | untried. | `untried` |
| 14 | **Hermes** | untried — version unknown | untried. Herdr's registry expects a **Python package** at `~/.hermes/plugins/herdr-agent-state/__init__.py`. | untried. | untried. | `untried` |
| 15 | **Qoder CLI** | untried — version unknown | untried. Herdr's registry expects `~/.qoder/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 16 | **Qwen Code** | untried — version unknown | untried. Herdr's registry expects `~/.qwen/hooks/herdr-agent-session.sh` (note the `-session` suffix: session-shaped reporting). | untried. | untried. | `untried` |
| 17 | **Mastra Code** | untried — version unknown | untried. Herdr's registry expects `~/.mastracode/hooks/herdr-agent-state.sh`. | untried. | untried. | `untried` |
| 18 | **Antigravity CLI** | untried — version unknown | untried. Herdr's registry expects `~/.gemini/config/hooks/herdr-agent-state.sh` — i.e. this CLI reads Gemini-shaped config under `~/.gemini/config`. Separate from Gemini CLI. | untried. | untried. | `untried` |

Rows 1–3 = 3 live-verified harnesses, rows 4–5 host-artifact, rows 6–18
untried with a documented hook point. 18 rows total.

**Named in the survey seed but in neither discovery source, so no row:**
`aider`, `crush`, `goose`, `amp`, `cline` (`continue` exists on `PATH` but is a
different tool), `openhands`. None appears in ROADMAP §§3.8–3.9, in
`herdr integration status`, or on this box — adding them would be a row with
nothing behind it. They are listed here so the omission is a decision.

## 3. Live-verified detail

### 3.1 pi 0.84.4 (`~/.bun/bin/pi`)

**State.** `--mode json` prints one NDJSON object per line. Verbatim shape from a
real run (`.loop/evidence/T-0075/pi-continue.txt`):

```json
{"type":"session","version":3,"id":"01a099df-e283-7631-9b1b-ccd20343cc95","timestamp":"…","cwd":"…"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
{"type":"message_end","message":{…}}
{"type":"message_start","message":{"role":"assistant","content":[…],"api":"openai-completions","provider":"verboo",…}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"OK"}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"stopReason":"stop"}}
{"type":"turn_end","…}
{"type":"agent_end","…,"willRetry":false}
{"type":"agent_settled"}
```

Failure shape is inside the same envelope (`stopReason:"error"`,
`errorMessage:"401 \"invalid or expired token\""`), so a native-tier integration
gets `blocked` with the provider's message without scraping (same transcript).

Extension hooks, read from herdr's *installed* pi extension
(`~/.pi/agent/extensions/herdr-agent-state.ts`, which is loaded by the live pi on
this box): `pi.on("session_start")`, `pi.on("agent_start")`,
`pi.on("agent_settled")`, plus a custom event bus (`pi.events.on("herdr:blocked")`).
The extension context exposes `ctx.sessionManager.getSessionId()`,
`ctx.sessionManager.getSessionFile()`, `ctx.mode` (`tui`/`print`/`json`/`rpc`) and
`ctx.isIdle()` — i.e. **a pi extension can hand Arreo the session id directly**,
which is exactly the pin T-0072 needs, without parsing stdout. pi's fork has a
wider hook set than pi's own extension happens to use —
`session_switch`, `before_agent_start`, `message_end`, `tool_call`,
`tool_execution_start/end`, `tool_approval_requested/resolved`, `session_shutdown`
— observed in the two extensions loaded into the live **omp** (`orca-agent-status.ts`,
`herdr-omp-agent-state.ts`); treat them as omp-verified and pi-plausible until a pi
extension uses them (row 3).

**Resume** (all four verified live):

| argv | observed |
| --- | --- |
| `pi --session-id 01a099ff-…-0001 --mode json -p "pinned one"` | prints `"id":"01a099ff-…-0001"`; creates the session under that id |
| …same argv, prompt `"pinned two"` | same id, **same file** — file contains both user messages; no second file created |
| `pi --session-dir DIR --session 01a099dc --mode json -p "second turn"` | resumes by **id prefix**; appends `"second turn"` to the original file |
| `pi -c -p "What is the code word?"` | reuses the newest session in cwd; answered `The code word is **BANANA**` from the prior turn |

Session store: `$PI_CODING_AGENT_DIR/sessions/<slug-of-cwd>/<ts>_<uuid>.jsonl`
(default agent dir `~/.pi/agent`); with `--session-dir` the file is flat in that
directory. The first line is `{"type":"session","version":3,"id":…}` — the
engine's `session_pattern` in `adapters/pi.toml` matches it.

**Custom models/providers.** `$PI_CODING_AGENT_DIR/models.json`:

```json
{"providers":{"verboo":{"baseUrl":"https://code.verboo.ai/router/v1",
 "api":"openai-completions","apiKey":"$VBK_TEST_KEY","authHeader":true,
 "models":[{"id":"deepseek-v4-flash-0731","name":"…","reasoning":true,
            "input":["text"],"contextWindow":1048576,"maxTokens":65536}]}}}
```

`apiKey` accepts a literal value, `$VAR`, and `${VAR}` — all three sent a working
request (`.loop/evidence/T-0075/pi-apikey-syntax.txt`). OAuth and other
credentials live in `$PI_CODING_AGENT_DIR/auth.json` (mode `0600`);
`pi auth check --provider verboo --json` → `{"status":"ready","provider":"verboo","authType":"api_key"}`.

### 3.2 opencode 1.18.30 (`~/.bun/bin/opencode`)

**State.** Two tiers, both verified. Universal: the pane text is the only signal
(`! permission requested: <scope> (<path>); auto-rejecting`, recorded in
`fixtures/opencode-question.pty`). Native: the plugin API, whose event set is
confirmed twice — by herdr's installed plugin
(`~/.config/opencode/plugins/herdr-agent-state.js`, `HERDR_INTEGRATION_VERSION=11`)
and by Orca's (`~/.config/orca/opencode-hooks/shared/plugins/orca-opencode-status.js`).
Both subscribe to the same names: `permission.asked`, `question.asked`,
`permission.replied`, `question.replied`, `question.rejected`, `tool.execute.before`,
`tool.execute.after`, `session.compacted`, `session.status`, `session.idle`,
`session.error`, `session.created`, `session.updated`, `session.deleted`.
`session.status` is a string or `{type}` whose values map
`idle→idle`, `active|busy|pending|retry|running|streaming|working→working`
(verbatim map in the two plugins). `--format json` events carry `sessionID`, e.g.
`{"type":"step_start","sessionID":"ses_f661e968dffeWZp8dXWhc6Bbbj",…}`.

**Resume.**

| argv | observed |
| --- | --- |
| `opencode run --pure -s ses_f661e968… --format json "second turn"` | all 6 events carry the same `sessionID` |
| `opencode run --pure -c --format json "third turn"` | same `sessionID` again — `-c` continues the last session **of the project id derived from the cwd** |
| `opencode session list --format json` | `[{"id":"ses_f661e968…","title":"New session - …","updated":…}]` |
| `opencode export ses_f661e968…` | full session JSON (`info.id`, `model{providerID,id}`, `tokens`, …) |

Store: SQLite **`$XDG_DATA_HOME/opencode/opencode.db`** — tables `session`,
`message`, `part`, `permission`, `project`, `project_directory`, plus
`credential` and `account`. Sessions are rows keyed by `project_id`, not files;
`opencode debug paths` prints the whole layout (`config`, `data`, `cache`,
`state`, `log`, `repos`, `bin`). The interactive TUI prints no id — ids are only
visible through `--format json`, `session list`, or the DB.

**Custom models/providers.** `$XDG_CONFIG_HOME/opencode/opencode.json{,c}`:

```jsonc
{"provider":{"verboo":{"name":"Verboo Code","npm":"@ai-sdk/openai-compatible",
  "options":{"baseURL":"https://code.verboo.ai/router/v1","apiKey":"{env:VBK_TEST_KEY}"},
  "models":{"deepseek-v4-flash-0731":{"id":"…","name":"…","tool_call":true,
             "interleaved":"reasoning_content","limit":{"context":1048576,"output":65536}}}}}}
```

- `{env:VAR}` is the documented env reference and is **load-bearing**: with
  `VBK_TEST_KEY` set the same config returned `OK`; with it unset the provider
  answered `401 {"error":"missing or invalid token"}`
  (`.loop/evidence/T-0075/opencode-env-subst.txt`).
- `opencode debug config` **resolves `{env:VAR}` to the literal secret in its
  output** — a debugging surface that leaks keys; never sync or paste its output.
- Merge order verified with a project-local file: global provider `verboo` +
  `<cwd>/opencode.json` provider `projmarker` → `debug config` lists both.

### 3.3 omp 18.1.16 (`~/.bun/bin/omp`)

Same lineage as pi (`PI_CODING_AGENT_DIR`, same session-file format, same
`--mode json` envelope — plus `advisor_cost_changed`), so the pi adapter's
shapes transfer; the differences that matter are listed here.

**Resume** (verified): `omp --session-dir D -r 01a099 --mode json -p "second turn"`
and `omp --session-dir D -c … "third turn"` both re-open the file
`2026-09-13T08-32-55-914Z_01a099e5-e66a-7568-9bd6-ac330e14b488.jsonl` and emit its
id in the session envelope; no new file. Store default
`~/.omp/agent/sessions/<cwd-slug>/`. `omp --from-claude` / `--from-codex` import
foreign session files (flag presence verified via `omp --help`).

**Custom models/providers.** `$PI_CODING_AGENT_DIR/models.yml` (probed with
`omp models verboo` → the six models from the scratch file). **`apiKey` env
syntax is `$VAR` only:**

| `apiKey` value | result |
| --- | --- |
| `$VBK_TEST_KEY` | `stopReason: "stop"` — works |
| `${VBK_TEST_KEY}` | `stopReason: "error"`, `errorStatus: 401` — the braces are sent literally |

That is a pi/omp divergence worth encoding per-adapter in config sync
(`.loop/evidence/T-0075/omp-apikey-syntax.txt`). `omp auth-broker` is a full
credential vault (`serve`/`token`/`login`/`import`/`migrate`/`status`), so omp
can also fetch keys at launch rather than read them from the file.

## 4. Host-artifact detail (CLI absent — proves the hook point, not the behaviour)

- **Codex.** Orca's shipped wiring writes a JSON hook config with eight events
  (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
  `PostToolUse`, `SubagentStart`, `SubagentStop`, `Stop`), each command hashed
  into a `[hooks.state."…":<event>:0:0] trusted_hash` block in the same
  directory's `config.toml` — i.e. Codex (as Orca drives it) treats hooks as
  code to be trusted, and `PermissionRequest` is a first-class event name. Orca's
  own `codex-hook.sh` lives at `~/.orca/agent-hooks/`. This is enough to design a
  Codex native-tier TOML; it is **not** enough to ship one, because no Codex
  binary was executed to confirm payloads or exit codes.
- **[CC].** Herdr's registry expects `~/.claude/hooks/herdr-agent-state.sh`;
  `~/.claude/` exists but is empty on this box. Nothing else to read.
- **Antigravity vs Gemini.** Herdr's `antigravity-cli` integration installs under
  `~/.gemini/config/hooks/` — Gemini-shaped config, different CLI. A future
  "Gemini" row must be recorded from a Gemini CLI run, not inferred from this.
- The same `herdr integration status` transcript is the source for every hook
  path in rows 7–18, and it also shows the *shape* of each harness's extension
  point: shell hooks for 12 of them, a JS plugin for **kilo**, a Python package
  for **hermes**, and a `-session`-suffixed script for **qwen**.

## 5. Cross-cutting findings

1. **BEL is not an attention signal in TUI mode** for pi (60/60 BELs are OSC
   terminators) or opencode (5/5). All three shipped adapters set
   `bell_means_attention = true`, and the engine scans raw bytes for `0x07`
   (`crates/arreo-core/src/state/engine.rs:191`), so a rendered hyperlink can
   flip a pane to `blocked`. The T-0017 fixtures could not catch it: they were
   recorded in `--print`/text mode. Detail and evidence: `tui-escape-analysis.txt`.
2. **Alt-screen separates the two live TUIs**: opencode enters it at startup, pi
   does not (default `--tui-mode regular`). Anything that keys "full-screen app"
   off `?1049` is wrong for pi.
3. **Env-var reference syntax is per-harness**: opencode `{env:VAR}` (verified),
   pi `$VAR` and `${VAR}` (both verified), omp `$VAR` only (verified — `${VAR}`
   is sent literally and 401s). Config sync (`arreo.toml`) has to translate or
   refuse per harness rather than assume one dialect.
4. **Session persistence differs in kind**: pi/omp write NDJSON `.jsonl` files
   per session (human-readable, greppable, cheap to back up); opencode writes
   rows into a multi-GB SQLite DB that also holds credentials. Resume tooling
   must not assume files.
5. **Both live harnesses self-report sessions to an extension/plugin** — pi gives
   `getSessionId()`/`getSessionFile()`, opencode gives `sessionID` on every
   event. Harness-aware resume (T-0072) is therefore *pin-and-read*, and the
   pin half is already verified for pi (`--session-id`).
6. **ACP is on the table for two of three live harnesses**: `opencode acp` starts
   an Agent Client Protocol server; `omp acp` runs one over stdio (both per
   `--help`). ROADMAP §3.9's ACP tier is reachable today for those two.

## 6. Evidence index (all under `.loop/evidence/T-0075/`)

| Transcript | Proves |
| --- | --- |
| `pi-help.txt`, `opencode-help.txt`, `omp-help.txt` | versions, flags, env-var lists, ACP/import commands |
| `pi-omp-subhelp.txt`, `opencode-subhelp.txt` | `pi auth`, `pi config`, `omp config/models/auth-broker`, `opencode run/session/providers/models/debug/acp/serve` |
| `opencode-paths.txt` | `opencode debug paths` layout; `debug info` shows the loaded plugin list |
| `pi-live-json.txt`, `pi-continue.txt`, `pi-live-ok.txt` | pi NDJSON envelope; `-c` restores history across runs (BANANA round-trip) |
| `pi-resume.txt`, `pi-pin3.txt` | pi `--session <prefix>` and `--session-id` pinning (one file, two turns) |
| `pi-apikey-syntax.txt`, `omp-apikey-syntax.txt` | pi `$VAR`+`${VAR}` vs omp `$VAR`-only |
| `opencode-custom-provider.txt`, `opencode-env-subst.txt`, `opencode-project-merge.txt` | custom provider registered; `{env:}` load-bearing; global+project merge |
| `opencode-live-run.txt`, `opencode-resume.txt`, `opencode-errors-export.txt` | `--format json` sessionID events; `-s`/`-c` resume; export; bad-model exit 1 |
| `opencode-db-tables.txt` + `session` table read | session/credential storage is SQLite rows |
| `opencode-json-vs-jsonc.txt` | `.json` and `.jsonc` in the same global config dir merge (both providers live) |
| `omp-live.txt`, `omp-resume2.txt`, `omp-config.txt`, `omp-xdg.txt` | omp models.yml read, session resume by prefix/`-c`, `omp config path`, `omp config init-xdg` |
| `omp-error-detail.txt`, `omp-acp-import.txt` | omp 401 shape; `omp acp`; `--from-claude/--from-codex` |
| `tui-escape-analysis.txt`, `pi-0.84.4-tui-10s.raw`, `opencode-1.18.30-tui-10s.raw` | the OSC/BEL/alt-screen findings |
| `herdr-integrations.txt` | hook/plugin entry point per harness (rows 4–18) |
| `host-integration-artifacts.txt` | Codex hook event names; Orca's omp/opencode event subscriptions |
| `absent-binaries.txt` | which harness CLIs are not installed (why rows are untried) |
| `secret-shape-scan.txt`, `existing-scan-secrets-e2e.txt` | the proposed rule extension, and the repo's real `scan_secrets` measured end to end through `arreo record` (false positive on `{env:VAR}`, miss on `token: vbk_…`) |
| `verify-adapters.txt` | `cargo xtask adapters --check` → `adapters: 15 passed, 0 failed`. The same file records the earlier run during which `adapters/pi.toml` + `adapters/opencode.toml` were mid-edit by T-0072 (a `[resume]` table header placed above the top-level `question_patterns` array, so TOML nested the arrays inside `[resume]`); the sibling fixed it and the check is green. No adapter file was edited by this task. |

## 7. What the matrix says to do next

- **Adapter-suite v2, batch A — the three live harnesses, done properly:**
  fix the BEL/OSC false-attention path first (finding 1), add omp as a
  first-class adapter (it is not one today: `adapters/` holds only `default`,
  `pi`, `opencode`), and record TUI-mode fixtures for pi and opencode (the
  current fixtures are `--print` only).
- **Adapter-suite v2, batch B — harnesses with a documented hook point:** Codex
  (eight event names already known) and [CC]. Both need a live recording session
  first; without one the honest output is `default.toml`, which already covers
  them.
- **Adapter-suite v2, batch C — the extension-family long tail** (kilo's JS
  plugin, hermes' Python package, qwen's session hook, and the shell-hook
  remainder): one TOML per harness *only* where a recording exists, otherwise
  nothing.
- **Config sync (§3.8)** consumes the design note
  (`docs/harness-centralization.md`) rather than this matrix's prose — but the
  scan changes in its §3.1 are the blocker: today the shipped scanner both
  misses `token: vbk_pro_…` and refuses a correctly converted
  `"apiKey": "{env:VAR}"`.
