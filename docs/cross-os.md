# Cross-OS strategy (T-0010)

> Decided: portability is proven in **layers**. Each layer states exactly what
> it proves — claiming more than the evidence is forbidden (PROMPT.md §6).

## The layers

| Layer | Command | Proves | Cost |
| --- | --- | --- | --- |
| 1. Local check | `cargo xtask check-targets` | Our Rust code has no cfg/type errors for `x86_64-pc-windows-msvc` (pure-Rust surface incl. tests) + native linux PASS | ~1 min, zero downloads (rustup std only) |
| 2. CI matrix | `.github/workflows/ci.yml` (`ubuntu/macos/windows`) | Real build + full tests + conpty-smoke on real OSes — **when it is green.** It is not green today: every run in the recorded history failed on all three legs | Free (public repo runners), per push |
| 3. Full SDKs | xwin (Windows) / osxcross (macOS) locally | C-dep compilation (rusqlite bundled) for foreign targets | GBs of SDK downloads — **deferred** (see below) |

**Layer 2 is the only layer that can prove runtime behavior on a foreign OS, and it
is currently red** — so macOS and Windows behavior is *unproven*, not proven-bad, and
no claim about either may be made from a green-looking workflow file. The matrix is
the authority once it passes.

## What check-targets does

- Full `cargo check --workspace --all-targets --target <t>` first (the real
  product surface). Green → PASS.
- If that fails **only** inside a C build script (missing linker/libs), retry
  the C-free surface (`arreo-core --no-default-features`, which drops the
  `sqlite` feature — its only C dep). Green → SKIP with the reason (not
  fake-red). Any rustc error in our code → FAIL, both passes.
- `--enforce` turns SKIP into failure: CI pre-merge will use it once the SDK
  steps land. Until then CI runs the gate without `--enforce`, and the OS
  matrix job is the authority for skipped targets.
- A deliberate `std::os::unix` import without `cfg` was planted during
  development and the gate FAILED it with the exact rustc error
  (`error[E0433]`); evidence in `.loop/evidence/T-0010/`. The gate is tested,
  not trusted.

## Why not full xwin/osxcross on the dev box today

- **xwin**: downloads a full Windows SDK (GBs) for one signal — C-dep
  compilation — that CI's `windows-latest` runner already provides free on
  every push. Cost/signal ratio is bad; revisit if CI minutes ever cost us.
- **osxcross**: needs an Apple SDK download with EULA-sensitive sourcing, plus
  toolchain assembly. Same CI argument (`macos-latest` is free and real).
- **Feature-gating note**: workspace-wide `--no-default-features` does NOT
  strip `arreo-core`'s defaults (dependents' plain `path` edges re-enable
  them via feature unification — verified during T-0010). Hence the lite
  pass is `-p arreo-core --no-default-features`, and `xtask`'s own edge is
  `default-features = false` (it never touches the store). If a future crate
  gains a second C dep, the lite pass must cover it too — the gate's SKIP
  message names the surface it verified.

## What each layer does NOT prove

- Layer 1 does **not** prove C deps compile for foreign targets, nor any
  runtime behavior. It proves our Rust is cfg-clean.
- **Wine** = quick checks only, never a shipping claim.
- **macOS on non-Apple hardware** (OSX-KVM-style VMs) violates Apple's EULA —
  we don't do it, and we don't fake macOS results: cross-compile proves *it
  builds*, CI runners prove *it behaves*.
