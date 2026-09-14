---
id: T-0099
title: `arreo plugin new` — and hot-reload that cannot wedge the UI
phase: 4
priority: 4
status: proposed
depends_on: [T-0098]
scope:
  - crates/arreo-cli/src/plugin.rs
  - crates/arreo-plugin-api/**
  - crates/arreo-server/src/daemon.rs
  - docs/plugins.md
  - .loop/evidence/T-0099/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice api
---

## Goal

ROADMAP §3.15: "a `arreo plugin new` scaffold + a typed host API makes a 'hello widget' a ~30-line
project", and "drop a `.wasm` into `~/.config/arreo/plugins/` → live reload, no daemon restart".

One sentence: a contributor gets a compiling plugin from one command, and an operator gets a
plugin reload that cannot take the daemon down.

## Acceptance criteria

- [ ] `arreo plugin new <name>` writes a compiling component project (Cargo.toml with
      `wasm32-wasip2`, a `plugin.toml` manifest with the smallest capability set, and a source
      file that implements the interface) and prints the two commands that build and install it.
      The generated project **builds**, asserted by running `cargo build --target
      wasm32-wasip2` on it in the test battery.
- [ ] Hot-load: dropping a component into the plugins directory loads it without restarting the
      daemon; removing it unloads it. A component that fails to load or traps on a call leaves
      the previously loaded version (or nothing) serving, and the failure is reported — the
      "remove it → gone, never wedged" property.
- [ ] A reload storm is safe: N rapid replacements leave exactly one loaded version and no
      leaked instance (asserted with a loop and a count, not by inspection).
- [ ] The daemon's plugin state survives its own restart (what is loaded is re-derived from the
      directory, so there is no second source of truth to drift).
- [ ] `docs/plugins.md` walks the whole path: `plugin new` → build → drop in → see it → remove
      it, with the capability list and what each one grants.

## Notes

- Prerequisite: T-0098's host and gate. This task adds the authoring surface and the reload
  lifecycle, not the sandbox.
