T-0082 — scope note: batch C recorded nothing, and here is why for each harness
================================================================================

OUTCOME: **N = 0 adapters** (a valid outcome per criterion 1, stated here as it
requires). No TOML written, no fixture synthesized. `cargo xtask adapters --check`
is green at **24 passed, 0 failed** — the count is unchanged because nothing was
added.

THE SHAPE OF THE BLOCKER
------------------------
None of the twelve harnesses is on this box (PATH probe, section 1 of
`transcript.txt`). Four of the twelve **are** published on npm as the real
product and were installed into scratch, and all four run — but none can drive a
turn, because this box holds no credential for any of them. The registry entry
herdr names is the **hook extension point**, not a runnable harness: a TOML
cannot be written from it, which is exactly why the survey called it a seed list.

PER-HARNESS LINES (criterion 2)
-------------------------------
Attempted and not recorded:

- **Grok** — binary absent. No package identity found: `@xai/grok-cli` is
  NOT-FOUND, and the name that resolves (`grok-cli` 1.0.5) is a different tool
  ("starts anthropic-proxy with Grok model and runs claude-code", repo
  `whitesmith/grok-cli`) — not the Grok CLI. Not attempted further.
- **Qwen Code** — binary absent; `@qwen-code/qwen-code` 0.23.3 installs and runs
  (`qwen --version` → `0.23.3`). No turn: *"No auth type is selected. Please
  configure an auth type (e.g. via settings or `--auth-type`) before running in
  non-interactive mode."* — **no credentials**.
- **Qoder** — binary absent; no package found under the probed names
  (`@qoder/qoder-cli` NOT-FOUND). Not attempted further.
- **Droid (Factory)** — binary absent; `@factory-ai/droid` NOT-FOUND. Not
  attempted further.
- **Cursor CLI (`cursor-agent`)** — binary absent. The npm name `cursor-agent`
  is a different tool ("Task sequence creator for Cursor AI agents", repo
  `zalab-inc/cursor_agent`), not Cursor's CLI, which is distributed outside npm.
  Not attempted further.
- **GitHub Copilot CLI** — binary absent; `@github/copilot` 1.0.83 installs and
  runs (`GitHub Copilot CLI 1.0.83`). No turn: *"Error: No authentication
  information found."* — **no credentials** (needs a GitHub OAuth/PAT token).
- **Devin** — binary absent; `@cognition-ai/devin` NOT-FOUND. Not attempted
  further.
- **Kimi Code** — binary absent; `@moonshot-ai/kimi-code` 0.42.0 installs and
  runs (`kimi --version` → `0.42.0`). No turn: *"No model configured. Run `kimi`
  and use /login to sign in, then retry; or set default_model in config.toml."* —
  **no credentials**.
- **Antigravity CLI** — binary absent; `@google/antigravity` NOT-FOUND. Not
  attempted further.
- **Mastra Code** — binary absent; `@mastra/code` NOT-FOUND. Not attempted
  further.
- **Kilo** — binary absent; `@kilocode/cli` 7.6.2 installs and runs
  (`kilo --version` → `7.6.2`). No turn: *"You need to sign in to use this
  model."* — **no credentials**.
- **Hermes** — binary absent. Herdr's registry expects a **Python package**
  (`~/.hermes/plugins/herdr-agent-state/__init__.py`), so npm is the wrong
  registry to probe; not attempted further.

Also true, and worth recording: the only agent CLIs installed on this box are
`pi`, `opencode` and `omp` — all three already have adapters (T-0017), so there
is no local harness left to record.

CRITERION 4 — the registry is not a deliverable
-----------------------------------------------
No adapter was written from a registry entry alone. The only TOMLs in
`adapters/` are the recorded ones (pi, opencode, omp, default), and
`cargo xtask adapters --check` proves they still parse and replay: 24 checks,
0 failures.

CRITERION 3 — the count
-----------------------
`adapters: 24 passed, 0 failed`. Stated because criterion 3 asks for it; the
number is *unchanged* from before this task, and that is the honest evidence
that nothing was fabricated to make the batch look productive.

FOLLOW-UP
---------
T-0089 — one retry task covering this batch and T-0081's, gated on a credential
appearing on the box. It carries the four verified versions and the codex/[CC]
resume argv recorded in T-0081's note, so the retry starts from a warm state
rather than re-running the discovery.