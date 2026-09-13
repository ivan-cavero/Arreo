# Harness config sync — what is safe to replicate, what must never leave a machine

One sentence: ROADMAP §3.8 says "add a provider once, replicated everywhere" —
this note is the part §3.8 leaves open, namely *which file, which field, which
path, and which secret rule* applies per harness, worked to the end on the
owner's real `opencode.jsonc`.

The mechanism itself (per-file version vectors, keep-both conflicts, rolling
history, `arreo sync revert`) is ROADMAP §3.8 and is **not** re-designed here.
This document is the preset data that mechanism needs plus the failure modes the
survey (T-0075, `specs/harness-matrix.md`) actually observed.

## 1. The three classes

Every candidate file lands in exactly one class. The class is per-file, decided
by a preset, never inferred from a folder.

| Class | Sync? | What it is | Rule |
| --- | --- | --- | --- |
| **SYNC** | yes, opt-in per file | Portable intent: provider/model lists, model roles, theme, non-secret options | Must pass the pre-sync secret scan (§3) and contain no absolute path. |
| **LOCAL** | never | Credentials, session state, caches, logs, daemon sockets, device ids, absolute paths | Hard deny-list, checked before the scan (so a hit is "this file is not syncable", not "this file is dirty"). |
| **PROJECT** | never (by this feature) | Files that belong to a repository (`<repo>/opencode.json`, `.pi/settings.json`, `AGENTS.md`) | They travel with git, not with the mesh; syncing them would fight the repo. |

Why the fence is per-file and not per-folder: opencode puts portable intent
(`opencode.jsonc`) and every credential plus a 2.9 GB of session state
(`opencode.db`) in two different XDG roots, while pi interleaves
`models.json` (portable) with `auth.json` and `sessions/` (never) *in the same
directory*. A folder rule would be wrong for both.

## 2. Per-harness inventory

Paths are symbolic (`$XDG_CONFIG_HOME`, `$PI_CODING_AGENT_DIR`, `%APPDATA%`) on
purpose: §3.11 gives each OS a different config root, so a preset stores the
symbolic location and resolves it on the receiving machine. Verified paths come
from `specs/harness-matrix.md` §3; unverified ones are marked.

### opencode 1.18.30 (verified)

| File | Class | Portable fields | Machine-local fields |
| --- | --- | --- | --- |
| `$XDG_CONFIG_HOME/opencode/opencode.jsonc` | **SYNC** | `provider.<id>.{name,npm,options.baseURL}`, `provider.<id>.models.*`, `plugin` (only with paths relative to the config dir), `agent`, `mcp` definitions that use no local paths | `provider.<id>.options.apiKey` (must be an env reference), any absolute path in `plugin`/`mcp` |
| `$XDG_CONFIG_HOME/opencode/tui.jsonc` | **SYNC** | TUI plugin/theme preferences, again only with relative paths | absolute paths |
| `$XDG_CONFIG_HOME/opencode/plugins/*` | **SYNC** | Hand-written plugin sources | — |
| `$XDG_CONFIG_HOME/opencode/node_modules`, `package.json`, `package-lock.json`, `bun.lock`, `.gitignore` | **LOCAL** | — | Reproducible from the plugin list; per-machine install artefacts (the real dir carries a checked-in `.gitignore` with exactly these entries). |
| `$XDG_DATA_HOME/opencode/opencode.db` (+`-wal`,`-shm`) | **LOCAL** | — | Sessions, messages, parts AND `credential`/`account` rows in one SQLite file. Never sync: it is state, it is huge, and half of it is secrets. |
| `$XDG_DATA_HOME/opencode/{log,snapshot,repos,storage,tool-output}`, `$XDG_CACHE_HOME/opencode`, `$XDG_STATE_HOME/opencode` | **LOCAL** | — | caches, logs, per-project snapshots |
| `<repo>/opencode.json` | **PROJECT** | — | Verified to merge *on top of* the global config: with both present, `debug config` listed the global `verboo` and the project `projmarker`. Two merge hazards: (a) a project file and the global file both defining the same provider id, (b) `.json` and `.jsonc` in the same global dir merge as well (verified) — the preset must warn when the sibling extension exists. |

### pi 0.84.4 (verified)

| File | Class | Portable fields | Machine-local fields |
| --- | --- | --- | --- |
| `$PI_CODING_AGENT_DIR/models.json` | **SYNC** | `providers.<id>.{baseUrl,api,authHeader,models[]}` | `providers.<id>.apiKey` when it is a literal; `$VAR`/`${VAR}` references are fine (both verified) |
| `$PI_CODING_AGENT_DIR/settings.json` | **SYNC** | `theme`, `packages` (npm sources), resource toggles set by `pi config` | `lastChangelogVersion` (per-machine noise; exclude) |
| `$PI_CODING_AGENT_DIR/auth.json` (mode 0600) | **LOCAL** | — | OAuth/API credentials. Never sync; never read by the sync engine. |
| `$PI_CODING_AGENT_DIR/{sessions,npm,bin,intercom,subagents.json,models-store.json,extensions,agents,chains}` | **LOCAL** | — | session state, installs, presence/sockets |
| `<repo>/.pi/settings.json` | **PROJECT** | — | project overrides (from `pi config -l`) |

### omp 18.1.16 (verified)

| File | Class | Portable fields | Machine-local fields |
| --- | --- | --- | --- |
| `$PI_CODING_AGENT_DIR/models.yml` | **SYNC** | `providers.<id>.{name,baseUrl,api,models[]}` incl. `compat.thinkingFormat`/`reasoningContentField` | `providers.<id>.apiKey` literal — and note **`$VAR` only**: `${VAR}` is sent literally and 401s |
| `$PI_CODING_AGENT_DIR/config.yml` | **SYNC** | `modelRoles.default`, `theme.dark`, `composer.shape` | — |
| `$PI_CODING_AGENT_DIR/{agent.db,agent.db-wal,history.db,models.db,blobs,sessions,terminal-sessions,logs,cache,run,last-changelog-version}` | **LOCAL** | — | sessions, credentials, caches, daemon run state |
| `$XDG_{DATA,STATE,CACHE}_HOME/omp` | **LOCAL** | — | created by `omp config init-xdg`; `omp config path` still reports `$HOME/.omp/agent` |

### Codex / [CC] / Gemini CLI / Grok CLI — **preset paths unverified**

No CLI for any of these exists on this box (`specs/harness-matrix.md` §4), so
there is no preset data here yet — only what a neighbouring product's wiring
implies:

- Codex: a TOML config with hook events `SessionStart`, `UserPromptSubmit`,
  `PreToolUse`, `PermissionRequest`, `PostToolUse`, `SubagentStart`,
  `SubagentStop`, `Stop` and per-hook `trusted_hash` entries (Orca's runtime
  home, read on this box). A synced Codex config must therefore **never** sync
  the `hooks.state.*.trusted_hash` blocks: they hash a local file, so they would
  diverge per machine and push a hook-trust prompt on every sync.
- [CC]: herdr expects its hook at `~/.claude/hooks/…`; `~/.claude/` exists and is
  empty. Config path unverified.
- Antigravity CLI reads Gemini-shaped config under `~/.gemini/config/` — do not
  reuse that path for Gemini CLI without a live check.

Until each of those has a recorded run, the honest preset is "not syncable" and
the file falls back to a user-declared custom path with a warning that the
secret scan is the only protection.

## 3. Secret rules — how a key never syncs

### 3.1 There is already a scanner, and its behaviour is now measured

`crates/arreo-core/src/fixtures.rs::scan_secrets` (used by `arreo record`, and by
the masker via the shared `find_token`) is the code that must be reused, not
replaced. Its rule set, read from source: `TOKEN_PREFIXES = [sk-, AKIA, ghp_,
gho_, xox]` with `MIN_TOKEN_RUN = 8`, `BEGIN … PRIVATE KEY` blocks, field-name
assignments (`api_key`, `apikey`, `api-key`, `aws_secret`, `client_secret`), and
`password`/`passwd` assignments ≥ 12 chars.

Run end to end against secret-shaped **dummies** through `arreo record`
(transcript: `.loop/evidence/T-0075/existing-scan-secrets-e2e.txt`):

| line fed to `arreo record` | scanner verdict | what it means for §3.8 |
| --- | --- | --- |
| `"apiKey": "vbk_pro_0123…"` | **refused** — `possible secret assignment (apikey)` | The owner's real configs are protected today: the *field name* catches `vbk_`, not any prefix rule. |
| `token: vbk_pro_0123…` | **saved** (no finding) | **Gap.** Same secret, field name the list does not know, no prefix rule for `vbk_` → it would sync. |
| `apiKey: xai-…` / `AIza…` / `eyJ….….…` | refused (via the field name) | Prefix coverage is missing for xAI/Google/JWT; only the field heuristic saves it. |
| `"apiKey": "{env:VBK_PROD_KEY}"` | **refused** — `possible secret assignment (apikey)` | **Blocker.** The correctly converted form is rejected: the heuristic fires on the field name regardless of the value. Sync would never accept a safe file. |
| `apiKey: sk-0123…` | refused, two findings (`sk- prefix` + assignment) | Prefix rules work when the prefix exists. |

So the work is not "write a scanner" but three concrete changes:

1. **Extend `TOKEN_PREFIXES`** with the prefixes this survey observed in the
   wild: `vbk_`, `xai-`, `glpat-`, `hf_`, `AIza`, plus a JWT shape. The
   negative control is already in hand: the `token: vbk_pro_…` line above must
   flip from *saved* to *refused*, and it must be a unit test with a real-shaped
   dummy. (A first sketch of the wider rule set is in
   `secret-shape-scan.txt`; there, the `vbk_` rule itself initially missed
   `vbk_pro_…` because it forbade `_` — the same class of mistake, caught by the
   same kind of control.)
2. **Teach the scan what a reference is** before it treats a value as a secret:
   a field whose value matches that harness's env-reference dialect (§3.2) is a
   reference, not a literal. Without this, change 1 makes the false positive in
   row 4 *more* likely, not less, and no converted file can sync.
3. **Keep the scan independent of the classification** (§2): the class decides
   whether a file may sync at all; the scan is the second net for a file that is
   allowed. A LOCAL-class file is refused by class, never "scanned and hoped".

Honest limit: no shape rule sees a key with an unrecognised prefix in an
unrecognised field. That is exactly row 2 of the table. The mitigation is the
whitelist in §2 — fields that are not registered as portable are refused — and
"add a prefix" every time a harness invents one.

### 3.2 Env-reference dialects (the escape hatch, per harness)

ROADMAP §3.8's escape hatch is `${ARREO_ENV:OPENAI_API_KEY}`, resolved on each
machine from its own keychain. Because each harness spells an env reference
differently — and one of them does not support the brace form at all — sync must
**translate** the Arreo form into the receiving harness's dialect:

| Harness | Native reference | Verified | Translation from `${ARREO_ENV:NAME}` |
| --- | --- | --- | --- |
| opencode | `{env:NAME}` | works; key absent → 401 (load-bearing, so it is genuinely reading the env) | `{env:NAME}` |
| pi | `$NAME` or `${NAME}` | both work (both sent a working request) | `${NAME}` |
| omp | the **bare variable name** | `$NAME` and `${NAME}` are sent literally as the bearer token → 401 (measured twice, the second time through a proxy logging the Authorization header; T-0080) | the bare name |
| others | unknown | untried | refuse until verified |

These dialects are the same table the scan needs (change 2 above), and the
receiver rewrites the Arreo form on arrival — so the file that lands on disk is
always native to that machine's harness, and the synced payload never contains
the harness-specific spelling either.

Value resolution: the value never enters a synced file. Arreo already spawns the
harness in a PTY, so the launch path can inject `NAME=<secret>` from the OS
keychain into the child's environment after reading the file; a harness launched
outside Arreo needs the variable exported in the user's shell profile, and the
sync output says so when a machine has the file but not the secret.

### 3.3 Two leak surfaces found while probing

- `opencode debug config` **resolves `{env:VAR}` to the literal value** in its
  output (`opencode-env-subst.txt`). It is a diagnostics command whose output
  must never be pasted into an issue, a synced file, or a session transcript.
- Session stores double as credential stores: `opencode.db` has
  `credential`/`account` tables, and pi keeps `auth.json` (0600) beside
  `models.json`. Any "back up my harness config" affordance must walk the SYNC
  class only.

## 4. The owner's case, worked end to end

> Owner's pain (§3.8): adding a provider means editing `opencode.jsonc` on every
> PC. Case: *one custom provider, edited once, on every machine.*

Machine set: `workbox` (Linux, owns the edit), `rpi5` (Linux), `mac` (macOS),
`desk` (Windows). All four run Arreo servers on the mesh; the file is opted in
once in `arreo.toml`:

```toml
[sync]
"opencode.jsonc" = { harness = "opencode", path = "$XDG_CONFIG_HOME/opencode/opencode.jsonc" }
```

(That table's *existence* is ROADMAP §3.8's "watched files, opt-in per file";
its exact keys are the implementation's choice — what matters here is that the
preset contributes the symbolic `path`, not the user.)

**Step 0 — once per machine, never synced.** Each machine stores the provider key
in its OS keychain and exports `VBK_PROD_KEY` for harnesses that read env:

```console
$ arreo secrets set VBK_PROD_KEY          # proposed verb (keychain bridge, §5.3) — does not exist yet
```

**Step 1 — the edit, on `workbox` only.** The real file on this box, after the
edit, is:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "verboo": {
      "name": "Verboo Code",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://code.verboo.ai/router/v1",
        "apiKey": "{env:VBK_PROD_KEY}"
      },
      "models": {
        "deepseek-v4-flash-0731": {
          "id": "deepseek-v4-flash-0731",
          "name": "Verboo deepseek-v4-flash-0731",
          "tool_call": true,
          "interleaved": "reasoning_content",
          "modalities": { "input": ["text"], "output": ["text"] },
          "limit": { "context": 1048576, "output": 65536 }
        }
      }
    }
  }
}
```

What the edit actually consists of: `apiKey` becomes an env reference (§3.2), and
everything else — `npm`, `baseURL`, the model list with its `limit`/`modalities`
— is portable intent. Nothing in the diff is machine-specific, which is the test
for "this belongs in the synced file".

**Step 2 — pre-sync scan.** This is the step that does not work today, and the
reason the scan needs the change described in §3.1. Two things are true at once:

- The file as shown is *correct* — but `scan_secrets` refuses it, because the
  `apikey` field heuristic fires on the field name and never looks at the value
  (`existing-scan-secrets-e2e.txt`, line 4). A reference-aware scan is required
  before any converted file can leave the machine.
- The *unconverted* file is refused too (same field heuristic) — but the same
  secret under a field name the heuristic does not know (`token: vbk_pro_…`) is
  **accepted and saved**. Trusting the field heuristic alone is not enough; the
  prefix rules have to grow.

With change 2 of §3.1 in place the scan reports `0 findings` for the file above
and the outbox gets a fresh version-vector entry for `workbox`.

**Step 3 — the mesh does the rest.** `workbox`'s counter for `opencode.jsonc`
bumps; the delta travels over the existing mesh (relay-routed or LAN-direct, the
same path `arreo machines` uses) and every peer converges. No central authority,
so `rpi5` can receive it while the relay is down.

**Step 4 — per-machine path resolution.** Each machine computes its own
destination — `$XDG_CONFIG_HOME/opencode/opencode.jsonc` on Linux,
`~/Library/Application Support/opencode/opencode.jsonc` on macOS, `%APPDATA%` on
Windows (ROADMAP §3.11) — which is why the preset stores the symbolic path. The
file's *contents* are byte-identical on all four; only the location differs. The
preset refuses to write when the destination's sibling extension exists
(`opencode.json` next to `opencode.jsonc` merges — verified), because two
provider lists would then both be live.

**Step 5 — verify on each machine.** Both halves are checkable from the transcript
of this survey:

```console
$ opencode models verboo
verboo/deepseek-v4-flash
verboo/deepseek-v4-flash-0731
…
$ opencode run --pure --format json -m verboo/deepseek-v4-flash-0731 "Reply with exactly: OK"
{"type":"step_start","sessionID":"ses_f661e968…",…}
{"type":"text",…,"part":{"type":"text","text":"OK",…}}
```

The first command proves the provider list replicated; the second proves the key
resolved *locally* (the identical config with the variable unset answered
`401 missing or invalid token`). A machine that replicated the list but has no
key fails the second command and says so — never silently.

**Step 6 — the conflict case.** `mac` also edits the same file (adds a second
provider) while `workbox` is offline. §3.8's rule applies unchanged: both are
kept, the loser is written as
`opencode.conflict-mac-20260913T101500.jsonc` next to the winner, and the
notification carries the diff. Two structural hazards specific to these files
are worth stating in the merge prompt: a JSON array key (`plugin`) merges as a
union rather than a replacement, and JSONC comments are legal in the real file
format, so the merge must either preserve them or refuse rather than reformat.
Until the chezmoi-style three-way merge of §3.8 lands, keep-both is the honest
behaviour.

**Step 7 — undo.** `arreo sync revert opencode.jsonc` restores the previous
version from the local SQLite history (§3.8). Providers broken at 3 a.m. is one
command; the not-synced classes are untouched by it.

**What never happens in any step:** a literal key crossing the wire; an absolute
path crossing the wire; the session database, `auth.json`, or any log being
considered; a project-local `opencode.json` being synced (step 4 refuses, and the
class table in §2 explains why).

## 5. What the implementation needs (inputs, not code)

1. **Preset registry** — one entry per harness: symbolic path(s) per class, the
   portable-field whitelist, the deny list, and the env reference dialect. §2 and
   §3.2 of this note *are* the first three entries (opencode, pi, omp); the rest
   stay out until each has a recorded run.
2. **Scanner** — extend the existing `arreo-core::fixtures::scan_secrets`
   (never a second scanner): new prefixes in `TOKEN_PREFIXES` (`vbk_`, `xai-`,
   `glpat-`, `hf_`, `AIza`, JWT), reference-awareness per harness dialect (§3.2),
   and a unit test per rule with a real-shaped dummy plus the two negative
   controls this survey produced — `token: vbk_pro_…` must flip from saved to
   refused, and `"apiKey": "{env:VAR}"` must stay clean.
3. **Keychain bridge** — read `NAME` per machine and inject it into the harness
   child's environment at spawn (Arreo owns the PTY); report when a machine has
   the file but not the secret.
4. **§3.8 wiring** — version vectors, conflict copies, notification with diff,
   rolling history and `arreo sync revert`, all referenced rather than restated.
5. **§3.11 path resolution** — the symbolic path expanded per OS, and the Windows
   case exercised on a real runner (this box cannot test it).

## 6. Unverified / open

- Codex, [CC], Gemini CLI and Grok CLI config paths and fields: **unverified** —
  no CLI on this box; a preset must not ship for them yet.
- macOS/Windows path expansion: from ROADMAP §3.11 only; not executed here.
- Whether a harness reads an env reference at launch time or at file-load time
  (opencode resolved `{env:}` at config load — verified; pi/omp resolved at
  request time in the probes) — matters for whether Arreo may inject the variable
  after the process starts. Only opencode's behaviour is firmly established.
- `opencode.json` vs `.jsonc` precedence is *merge*, not precedence (verified with
  both present) — but whether that holds across all providers/keys is untried.
- The scan changes in §3.1 are **designed, not implemented**. `secret-shape-scan.txt`
  is a Python sketch of the wider rule set, *not* the repo's Rust scanner; the
  authoritative measurements of the shipped scanner are the `arreo record` runs in
  `existing-scan-secrets-e2e.txt`. Both would need re-running against the real
  implementation before the task that ships them can claim a green control.
- Nothing in this note was exercised on a second machine: the mesh, the conflict
  copy and `arreo sync revert` are §3.8's design, described here only through the
  per-machine path/key resolution that the sync must perform.

## 7. Implementation notes (T-0083, the local half)

The design above is T-0075's. This section records what T-0083 actually shipped
and where the implementation differs from the prose — the prose is the survey's
reading, the code is the measured behaviour, and where they disagree the code
won (each disagreement is called out).

**What shipped, and where.** `crates/arreo-core/src/sync/` is the mechanism:
`presets.rs` (the registry of §2), `paths.rs` (symbolic-path resolution, §3.11),
`vectors.rs` (per-file version vectors), `merge.rs` (keep-both conflicts and the
three hazards), `keychain.rs` (the bridge), and `engine.rs` (the flow, over the
machine's own store — the vectors and the history live in the daemon's SQLite
store, schema v9). The CLI verbs are `arreo sync …` (`list`, `push`, `payload`,
`apply`, `history`, `revert`, `conflicts`, `merge`, `env`, `secret set|list`).
`cargo xtask sync --check` drives the §4 story on two isolated roots.

**The class check runs before the scan, and the scan is reused.** A LOCAL file
is refused as "not syncable", before it is read — `auth.json` does not even have
to exist for the refusal. The scan is `fixtures::scan_secrets`, never a second
scanner; the neutral reference form `${ARREO_ENV:NAME}` is translated to a
harness spelling *for the scan* so the reference predicate can see it (the file
on disk is untouched). A user-declared path gets the LOCAL deny-list of §1's
rule set as code: credential-store names, `*.db`/`*.sqlite` and sidecars,
`sessions`/`node_modules`/`cache`/`logs`/`blobs`/`run` components, logs and
locks, key material, and Codex `hooks.state.*.trusted_hash` blocks (a content
rule, refused before the scan because it is untransferable rather than dirty).

**The omp dialect correction.** §3.2's table says omp's native reference is
`$NAME`. T-0080 measured the header omp actually sends and both sigil forms are
forwarded verbatim as the bearer token (401); the working form is the bare
variable name. The preset follows the measurement: omp's dialect is the bare
name, and the payload's neutral form lands as the bare name on an omp machine.
The scanner's `is_env_reference` already documented this; the doc's §3.2 row is
the stale one.

**The residual is closed here, not in the scanner.** `is_env_reference` calls
an all-uppercase literal a reference (deliberately — the bare name is omp's
dialect). The sync path closes it: `receive` refuses a file whose references
this machine cannot resolve, **by name**, with the command that fixes it. A
literal that only looks like a name therefore cannot ride along, and a real
reference on a machine without the value is an actionable message instead of
the provider's 401 (T-0075 measured that 401).

**The neutral payload.** A payload carries the file with every reference in the
neutral `${ARREO_ENV:NAME}` form, so the bytes on the wire are not any one
machine's spelling; the receiver rewrites them into its own dialect before the
file lands. Normalisation is pure text substitution over value spans (comments,
key order and formatting survive), which is also how the absolute-path rule
reads "a value".

**Keep-both, not newest-wins.** A concurrent payload is written as
`<name>.conflict-<machine>-<ts>.<ext>` beside the untouched live file, and the
losing bytes are recorded in the local history (reason `conflict`), so
consuming the copy later loses nothing. One case a vector alone gets wrong is
protected too: a version vector counts *published* revisions, so before
anything is written the live file's bytes are checked against the newest
recorded revision — a live file the history has never seen (an edit made here
and not yet pushed, or a hand-written config) takes the keep-both path even
when the payload is strictly newer. `arreo sync merge <file>` is the explicit
reconciliation: provider objects merge, `plugin` arrays union (sets,
not replacement), a scalar both sides changed refuses with the key's path, a
commented document refuses (this merge re-emits what it parsed, and dropping
the operator's comments silently is the one outcome worse than refusing), and a
sibling `opencode.json` is folded in and renamed aside (`.reconciled-<ts>`,
never deleted) after the write gate has refused it.

**The write gate.** Writing `opencode.jsonc` while `opencode.json` exists is
refused, because opencode merges both (verified). The gate is data: the preset
carries `sibling_merges`, so a harness that does not merge siblings is never
given the rule.

**The keychain bridge.** The file carries the name; the value is injected at
PTY spawn from the machine's own store (a 0600 `$XDG_CONFIG_HOME/arreo/
secrets.json`, written by `arreo sync secret set NAME` with the value on stdin,
never argv and never echoed). The injection itself is the environment a spawn
applies — `keychain::plan(...).environment()` — and the slice proves it by
spawning a real pty child per root and observing each root's own value. The
daemon's spawn site (`arreo-server`) is T-0086's to wire; the mechanism is here
and `arreo sync env <file>` reports, by name, which variables this machine has
and which it lacks.

**What is T-0086's.** The transport: carrying `SyncPayload` over the mesh, the
outbox, watching files, and conflict copies across two live machines. The
exchange itself is a local function (`payload`/`receive`), so the slice already
runs the whole §4 story on two isolated roots with no network.
