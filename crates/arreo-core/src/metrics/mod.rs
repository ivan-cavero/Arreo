//! Metrics sampler (T-0006): per-process-tree RAM/CPU + SQLite rollups.
//!
//! One sentence: sample any pane's process tree from `/proc` in microseconds,
//! persist 10 s rollups to SQLite (WAL), prune on a retention window.
//!
//! Layout: `Sampler` owns the cross-platform trait (`Probe`); `platform`
//! holds the per-OS implementation (linux real, windows/macos honest stubs
//! until T-0010/T-0019 need them). `Store` owns the schema (versioned from
//! day one — migrations, not ad-hoc DDL — per T-0018's future).

pub mod platform;
pub mod sampler;
#[cfg(feature = "sqlite")]
pub mod store;

pub use platform::Probe;
pub use sampler::{ProcessSample, Sampler};
#[cfg(feature = "sqlite")]
pub use store::{Rollup, Store};
