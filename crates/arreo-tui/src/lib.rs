//! arreo-tui: sidebar + pane wall over the msgpack socket API (T-0015).
//!
//! Read-only v1: renders daemon truth (panes/read/metrics/state), sends
//! input on Enter. Theming is a minimal `Theme` trait surface here — the
//! full JSON engine lands in T-0016 without changing these call sites.

pub mod client;
pub mod model;
pub mod settings;
pub mod theme;
pub mod ui;
