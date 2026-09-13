---
id: T-0083
title: Harness config sync — presets, a reference-aware secret scan, and the keychain bridge (ROADMAP §3.8)
phase: 4
priority: 2
status: proposed
depends_on: [T-0075, T-0037]
scope:
  - crates/arreo-core/**
  - crates/arreo-server/**
  - crates/arreo-cli/**
  - xtask/src/**
  - docs/harness-centralization.md
  - .loop/evidence/T-0083/**
verify:
  - cargo xtask sync --check
  - cargo test --workspace
---

## Goal

ROADMAP §3.8's owner case, made mechanical: *edit one `opencode.jsonc` custom provider once, and
every machine has it.* T-0075 measured the ground truth this task builds on — which files are
portable intent and which are machine-local, per harness, from the live CLIs
(`docs/harness-centralization.md`, rows traced to transcripts) — and it found the blocker that
makes the naive version dangerous.

**The blocker, measured through the shipped scanner** (`.loop/evidence/T-0075/existing-scan-secrets-e2e.txt`,
via `arreo record`): `crates/arreo-core/src/fixtures.rs::scan_secrets` **refuses the correct
configuration**. `"apiKey": "{env:VBK_PROD_KEY}"` is reported as a secret, because the heuristic
fires on the field name and never looks at the value; and it **misses** `token: vbk_pro_…`,
because there is no rule for that prefix. So today the sync would reject exactly the files §3.8
tells the operator to write, while letting a real key through under a different field name.
Fixing the scanner is the first deliverable, not a follow-up.

## Acceptance criteria

- [x] **Reference-aware scanning** — landed (`e5f355f`): `is_env_reference` (opencode `{env:NAME}`, pi `$VAR`/`${VAR}`, omp the bare name), the measured prefixes (`vbk_`, `xai-`, `glpat-`, `hf_`, `AIza`, JWT) with per-prefix minimum runs, and the masker sharing the predicate. Both negative controls measured through `arreo record` before/after, and both halves mutation-checked (`.loop/evidence/T-0083/scan-negative-controls.txt`). Residual stated there: an all-uppercase literal with no provider prefix is called a reference — closed by the sync path refusing an unresolved name, not by more scanning.

**Re-scope (2026-09-13).** The task was one worker-session for four deliverables; the
scanner (above) landed here, and the **transport half is split out to T-0086**: exchanging
deltas with a *real* peer over the mesh, conflict copies across two live machines, and the
JSONC/array-merge hazards, which need the relay fabric and a protocol decision rather than a
local mechanism. What stays here is the local half — presets, the LOCAL deny-list, version
vectors and history, `arreo sync revert`, the keychain bridge, and `xtask sync --check`
driving the §3.8 worked case on two **isolated roots** standing in for two machines (which is
what criterion 5 asks for, and needs no network).

- [ ] **Per-file presets.** In a portable field, an env reference is not a secret —
      `{env:NAME}` (opencode), `$NAME` and `${NAME}` (pi), `$NAME` (omp) per the dialects T-0075
      verified — while a literal in the same field is. `TOKEN_PREFIXES` gains the measured
      shapes (`vbk_`, `xai-`, `glpat-`, `hf_`, `AIza`, plus a JWT shape). Both negative controls
      from T-0075's evidence become tests: `token: vbk_pro_…` must flip refused; `"apiKey":
      "{env:VAR}"` must stay clean.
- [ ] **Per-file presets**, per `docs/harness-centralization.md`: SYNC files (opencode
      `opencode.jsonc`/`tui.jsonc`, pi `models.json`/`settings.json`, omp `models.yml`/`config.yml`)
      with symbolic paths resolved per machine; LOCAL files (credentials, `*.db`, `sessions/`,
      caches, logs, `node_modules`, Codex `hooks.state.*.trusted_hash`) refused **before** the
      scan. A machine never receives a file it cannot use: presets are per-harness, not blanket.
- [ ] **Keychain bridge.** The synced file carries `NAME`; the secret is injected at PTY spawn
      from the machine's own store, and a machine that has the file but not the secret says so
      by name rather than failing opaquely at the harness (T-0075 measured: an unset key is a
      live 401 from the provider, never a config error).
- [ ] **Conflict and history**: version vectors, keep-both with the loser as
      `<name>.conflict-<machine>-<ts>.<ext>`, and `arreo sync revert <file>` from the local
      store (§3.8; the mechanism itself is §3.8's design, not re-invented here).
- [ ] **`xtask sync --check`** drives the whole story on two isolated roots standing in for two
      machines — the §3.8 worked case end to end (edit once → sync → both machines resolve the
      reference, one key present, one absent-and-said-so) — with evidence under
      `.loop/evidence/T-0083/`.
- [ ] The merge hazards T-0075 verified are handled, not discovered later: opencode's
      `opencode.json` **and** `opencode.jsonc` both merge (writing one while the sibling exists
      must refuse or reconcile, never silently diverge), JSONC comments must survive a merge or
      the merge must refuse, and array keys (`plugin`) merge as a union.

## Notes

- Inputs: `docs/harness-centralization.md` (the design + the per-file inventory),
  `specs/harness-matrix.md` rows 1–3, `.loop/evidence/T-0075/secret-shape-scan.txt` and
  `existing-scan-secrets-e2e.txt`.
- Only opencode/pi/omp get presets — the three with live-verified paths and dialects. A preset
  for a harness nobody recorded would be a guess wearing a schema.
- Rejected: a generic "sync a folder" mode (§3.8 explicitly forbids it — surprise overwrites),
  and storing secrets in the synced file with encryption (the key would then travel with the
  ciphertext, which is the same leak with more steps).
