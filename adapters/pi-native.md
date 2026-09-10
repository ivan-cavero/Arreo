# Pi native hook spec (T-0017) — event map for a future hook.
#
# pi supports `--mode json` (envelope observed on this box: `session`,
# `agent_start`, `turn_start`, `message_start`, `message_update`×N,
# `message_end`, `turn_end`, `agent_end`, `agent_settled`) and `--mode rpc`.
#
# Proposed map (to verify against a live hook when the daemon side lands):
# | pi event | Arreo state |
# |---|---|
# | `agent_start` / `turn_start` | `working` |
# | `message_start` (assistant, tool calls present) | `working` |
# | `message_update` (assistant text, ends with `?` + action verb) | `question (inferred)` — text-level, same as universal |
# | `turn_end` with pending permission | `question` (needs hook confirmation) |
# | `agent_end` / `agent_settled` | `done` (with exit semantics) |
# | process exit | `done` (code) |
#
# Until the hook exists, pi.toml (universal) is the tested path: every shape
# in it comes from recorded pi runs (fixtures/pi-*.pty).
