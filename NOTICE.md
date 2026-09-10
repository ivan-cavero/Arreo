# NOTICE

This repository contains software distributed under two licenses:

- **Apache License 2.0** — the Arreo runtime (`crates/arreo-core`, `crates/arreo-server`,
  `crates/arreo-cli`, `crates/arreo-plugin-api`, `crates/arreo-tui`, the mobile and web
  clients, plugin API, adapters, themes). See [LICENSE](LICENSE).
- **GNU Affero General Public License v3.0 or later** — the Arreo relay
  (`crates/arreo-relay*`). See [LICENSE-RELAY](LICENSE-RELAY).

## Third-party components

Arreo depends on outstanding open-source software; each dependency keeps its own license and
copyright. The notable ones:

| Component | License | Use |
| --- | --- | --- |
| [alacritty_terminal](https://crates.io/crates/alacritty_terminal) | Apache-2.0 | terminal emulation |
| [portable-pty](https://crates.io/crates/portable-pty) | MIT | PTY management (WezTerm) |
| [ratatui](https://ratatui.rs) / [crossterm](https://crates.io/crates/crossterm) | MIT | TUI framework |
| [quinn](https://github.com/quinn-rs/quinn) (QUIC) | Apache-2.0 / MIT | transport |
| [rust-noise](https://docs.rs/noise-framework) | Apache-2.0 / MIT | Noise protocol framework |
| [uniffi](https://github.com/mozilla/uniffi-rs) | MPL-2.0 | mobile bindings |
| [wasmtime](https://wasmtime.dev) | Apache-2.0 WITH LLVM-exception | plugin runtime |
| [rusqlite](https://crates.io/crates/rusqlite) (SQLite) | MIT / blessing | storage |
| [tokio](https://tokio.rs) | MIT | async runtime |

A complete, machine-generated inventory (`cargo deny` / SBOM) ships with every release at
`https://arreo.dev/notices/<version>` and in each release archive.

## Trademarks

"Arreo", the Arreo logo, and "Herd them from your phone" are trademarks of the Arreo
project. Forks must rename: see `docs/forking.md`.
