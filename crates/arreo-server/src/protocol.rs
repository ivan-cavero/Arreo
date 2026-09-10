//! v0 wire protocol: framed MessagePack `Message` (T-0014 cutover).
//!
//! The T-0005 JSONL framing is gone — one framing, not two. This module
//! re-exports the canonical types for daemon consumers.

pub use arreo_core::proto::codec;
pub use arreo_core::proto::{AgentState, Message, PaneInfo, VERSION};
