//! Lifecycle: re-exported from arreo-core (T-0012 dependency-direction
//! fix — the CLI needs unit-file types without depending on the server).

pub use arreo_core::lifecycle::{
    unit_file, unit_path, DrainReport, ServiceKind, SHUTDOWN_DEADLINE,
};
