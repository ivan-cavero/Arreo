//! v0 wire protocol: re-exported from arreo-core (T-0005 dependency-direction
//! fix — the gate caught arreo-cli depending on arreo-server for these types).
//!
//! T-0013 replaces the wire format with versioned MessagePack; the types move
//! with it. Until then, exactly one definition lives in `arreo_core::proto`.

pub use arreo_core::proto::{PaneInfo, Request, Response, VERSION};
