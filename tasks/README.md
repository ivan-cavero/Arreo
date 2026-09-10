# tasks/ — machine-readable task queue

Every task is one file: `T-<number>-<slug>.md` with YAML frontmatter. The loop (PROMPT.md)
picks the highest-priority task with `status: todo` whose `depends_on` are all `done`.

Schema:

```yaml
id: T-0001            # stable, never reused
title: "..."          # imperative, one line
phase: 0              # roadmap phase
priority: 1..5        # 1 = do first
status: todo          # todo | in-progress | done | needs-criteria | needs-human | parked
depends_on: []        # task ids
scope: []             # allowed paths/crates — the fence
verify: []            # commands that must be green before done
```

Rules (see PROMPT.md §3): criteria are written before work starts; evidence (tests, e2e
slices, bench numbers, screenshots) goes to `.loop/evidence/<id>/` and is referenced in the
file's `evidence` list when closing. `done` without evidence is not `done`.

## The loop owns the backlog — with judgment

The queue is a **garden, not a wall**. The loop agent (PROMPT.md §3) doesn't just consume
these files; it shapes them:

- **Creates tasks** when it finds gaps, broken assumptions, or next steps nobody wrote —
  criteria first, then the file (`T-0NNN-slug.md`, next free number, `proposed` status),
  then work. Planning passes decompose a whole phase with **concrete numbers**
  ("20–100 tasks", per the Cursor research) — vague "generate some tasks" produces few.
- **Splits** oversized tasks (one worker-session each is the target size) and **merges**
  ones that turned out trivial, with a ledger note explaining the judgment call.
- **Re-scopes** when reality disagrees with the original scope — with the reason written
  into the task file, never silently.
- **Retires** stale ones (`status: obsolete` with one-line why) — a stale task worked on
  out of respect for the plan is worse than acknowledging the plan moved.

Human review (or native review) is the gate on what *merges*; the backlog itself is the
agent's to garden.

## Cross-OS testing strategy (dev runs on Linux)

The dev loop runs on Linux (the VPS). Windows/macOS are proven, in order of strength:

1. **Cross-compile gates, every commit (local, fast):** `cargo-xwin` for
   `x86_64-pc-windows-msvc` (xwin sysroot) and **osxcross** for
   `aarch64/x86_64-apple-darwin` — build + clippy for all targets so portability errors
   surface in minutes, not in CI.
2. **Real behavior on GitHub-hosted runners (CI):** ubuntu/macos/windows jobs run the full
   battery — the only sanctioned way to *execute* macOS behavior, and free & unlimited on
   **public** repos (private repos burn paid minutes: macOS has a 10× multiplier).
3. **Wine** (cargo-xwin's image ships it): acceptable for quick, low-fidelity Windows
   checks on Linux — never a shipping claim. ConPTY behavior itself needs real Windows
   (the T-0007 smoke test runs on a Windows runner).
4. **Self-hosted runners** on your own machines (a Mac mini, a Windows box) for deep
   passes — optional, added later.

Running macOS on non-Apple hardware (OSX-KVM-style VMs) violates Apple's EULA — we don't
do it, and we don't fake macOS results: cross-compile proves *it builds*, CI runners prove
*it behaves*. Claiming more than the evidence is exactly what PROMPT.md §6 forbids.

Current queue: **Phase 0 — spike ("prove the daemon")** (T-0001…T-0009), plus early
Phase 1 preview tasks (T-0010+).
