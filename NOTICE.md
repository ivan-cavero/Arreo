# NOTICE

This repository contains software distributed under two licenses:

- **Apache License 2.0** — the Arreo runtime: `crates/arreo-core`,
  `crates/arreo-server`, `crates/arreo-cli`, `crates/arreo-tui`,
  `crates/arreo-plugin-api`, the adapter definitions in `adapters/`, the theme
  files in `crates/arreo-core/themes/`, and the dev tooling in `xtask/`. See
  [LICENSE](LICENSE).
- **GNU Affero General Public License v3.0 or later** — the Arreo relay
  (`crates/arreo-relay`). See [LICENSE-RELAY](LICENSE-RELAY).

The mobile and web clients scheduled by ROADMAP §6 are intended to be Apache-2.0
too, but they are not in this repository yet — nothing here licenses code that
is not here.

## Third-party components

Arreo depends on outstanding open-source software; each dependency keeps its own
license and copyright. The notable direct dependencies of this tree, with the
license each crate declares today:

| Component | License | Use |
| --- | --- | --- |
| [alacritty_terminal](https://crates.io/crates/alacritty_terminal) | Apache-2.0 | terminal emulation |
| [portable-pty](https://crates.io/crates/portable-pty) | MIT | PTY management (WezTerm) |
| [ratatui](https://ratatui.rs) / [crossterm](https://crates.io/crates/crossterm) | MIT | TUI framework |
| [quinn](https://github.com/quinn-rs/quinn) (QUIC) | MIT OR Apache-2.0 | transport |
| [snow](https://github.com/mcginty/snow) | Apache-2.0 OR MIT | Noise protocol framework |
| [rusqlite](https://crates.io/crates/rusqlite) | MIT | storage (the bundled SQLite is public domain) |
| [tokio](https://tokio.rs) | MIT | async runtime |

Two dependencies belong to later phases and are **not** in this tree today:
[uniffi](https://github.com/mozilla/uniffi-rs) (MPL-2.0) for the mobile bindings
(ROADMAP Phase 3) and [wasmtime](https://wasmtime.dev) (Apache-2.0 WITH
LLVM-exception) for the plugin runtime (Phase 4). Their notices land with them.

License compliance is checked by `cargo deny check` (configured by `deny.toml`, run as the
supply-chain step of `.github/workflows/ci.yml`), and every dependency's license is
recorded in `Cargo.lock` — that pair is the inventory of record today. There are no
releases yet and no `arreo.dev`; the machine-generated inventory (cargo-deny's `licenses`
output, or an SPDX SBOM) is an artifact the first release must ship in its archive.

## Trademarks

"Arreo" and the Arreo logo are the project's marks. Neither license grants
trademark rights — Apache-2.0 §6 permits only the customary use needed to
describe the origin of the work — so a fork that distributes Arreo must rename.
The license boundary a fork has to respect is stated in
[CONTRIBUTING.md](CONTRIBUTING.md) ("Working on the relay (AGPL)") and in
ROADMAP §7.
