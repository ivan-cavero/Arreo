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

Current queue: **Phase 0 — spike ("prove the daemon")**.
