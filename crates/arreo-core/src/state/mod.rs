//! State engine v0 (T-0004): universal-tier detection from PTY output alone.
//!
//! One sentence: bytes in, timestamped state events out — with an explicit
//! clock so every timeline is deterministic and replayable.
//!
//! Rules (ROADMAP §3.9 universal tier):
//! - Output flowing → `working` (immediately, latency ≈ 0).
//! - Silence + prompt-shaped tail → `question (inferred)` with the matched
//!   pattern (never claimed certain — the label says inferred).
//! - BEL (`\x07`) → attention immediately (`question` if the tail looks like
//!   a prompt, else `blocked`).
//! - Error shape + silence → `blocked`.
//! - Silence + plain tail → `idle`.
//! - Child exit → `done` with the code (any code — non-zero is still done;
//!   `blocked` is an output shape, not an exit status).
//! - Nothing seen yet → `unknown` (never lie).
//!
//! Clock discipline: every method takes `now_ms`. The engine never reads the
//! wall clock — tests and fixture replays drive time explicitly, so the exact
//! event timeline is assertable. Latency budget (≤ 200 ms) holds by
//! construction: output-driven transitions emit at the feed's timestamp.

pub mod adapter;
pub mod engine;

pub use adapter::Adapter;
pub use engine::{Confidence, Engine, Event, State};
