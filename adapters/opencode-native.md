# opencode native hook spec (T-0017) — how a plugin reports rich state.
#
# Proven pattern: herdr's live plugin (`~/.config/opencode/plugins/`) maps
# opencode events to states. An Arreo plugin does the same, reporting
# `StateEvent` over the socket API (T-0014) with the payload attached.
#
# Event → state map (verified against herdr-agent-state.js on this box):
# | opencode event | Arreo state | payload |
# |---|---|---|
# | `permission.asked` | `question` (native, NOT inferred) | tool + scope + path |
# | `question.asked` | `question` (native) | question text + options |
# | `permission.replied` / `question.replied` / `question.rejected` | `working` | — |
# | `tool.execute.before` / `tool.execute.after` | `working` | tool name |
# | `session.status` (active/busy/pending/retry/running/streaming/working) | `working` | — |
# | `session.status` (idle) | `idle` | — |
# | `session.idle` | `idle` | — |
# | `session.error` | `blocked` | error text |
# | `session.compacted` | `working` | — |
#
# Universal fallback (no plugin): the `! permission requested:` terminal line
# (see opencode.toml) still yields `question (inferred)`. Native replaces the
# label with `question (native)` + the actual payload — the single most
# valuable detection in the product (ROADMAP §3.9 flagship moment).
