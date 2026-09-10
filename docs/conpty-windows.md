# Windows / ConPTY notes (T-0007)

> Scope: what we know from code + unix-side evidence, and what only the
> Windows CI runner can prove. No Wine claims, no faked results.

## What runs where

- `cargo run -p xtask -- conpty-smoke` on **Windows** exercises the real
  ConPTY backend: `native_pty_system()` is type-aliased to
  `win::conpty::ConPtySystem` (portable-pty 0.9), so `Pane::spawn` goes
  through `PsuedoCon` — there is no second code path on our side.
- On **unix** the same verb runs the same `Pane` mechanics via `sh`
  (posix_openpt backend). This proves our layer, not ConPTY — the Windows
  CI run is the ConPTY proof.

## Known limitations (conhost degradation path)

- ConPTY requires Windows 10 1809+. Older builds: `openpty` fails at spawn;
  the smoke reports `spawn failed` (clean error, never a hang).
- UTF-8: ConPTY inherits the console codepage. The smoke forces
  `chcp 65001` before asserting multibyte output; without it, legacy
  conhost (non-Terminal) sessions mangle non-ASCII. Windows Terminal is the
  first-class target (truecolor + mouse); conhost gets correct text with
  quantized color (T-0016 owns the quantization).
- Resize maps to `PsuedoCon::resize` (no SIGWINCH — the console API instead);
  rapid resize spam is bounded by the same drain mechanics as unix.
- Kill maps to the ConPTY child teardown; the smoke asserts reap-or-error
  (no zombie console windows lingering after the run).

## If the Windows CI run fails

That is the task working as designed (fail-fast): paste the runner log into
a new task with the exact error, and do not hand-wave it as "CI flake" —
ConPTY differences (codepage, `cmd` echo behavior, `\r\n` shapes) are the
most likely culprits, and the smoke markers (`SMOKE-DIR-OK`, `héllo`) are
chosen to localize them.
