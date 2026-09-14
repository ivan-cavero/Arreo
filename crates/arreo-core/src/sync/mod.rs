//! Harness config sync (T-0083): the local half of ROADMAP §3.8.
//!
//! One sentence: edit one `opencode.jsonc` custom provider once, and every
//! machine has it — because a preset decides which files may travel and where
//! they live on each machine, the synced file carries a variable's *name*
//! instead of a key, every revision is kept locally, and two machines that
//! edited the same file keep both copies rather than one silently winning.
//!
//! ## The pieces, and what each one refuses to do
//!
//! - [`presets`] — the registry: SYNC files (portable intent), LOCAL files
//!   (credentials, sessions, caches, logs — refused **before** the scan, so a
//!   credential file is "not syncable" rather than "dirty"), and PROJECT files
//!   (they travel with git). Only opencode, pi and omp have entries, because
//!   only those three had their paths and dialects measured.
//! - [`paths`] — symbolic paths (`$XDG_CONFIG_HOME`, `$PI_CODING_AGENT_DIR`,
//!   `%APPDATA%`) resolved per machine, so the file's bytes are identical
//!   everywhere while its location is not. An unset root is named, never
//!   guessed.
//! - [`vectors`] — per-file version vectors: who has seen what, so a receiver
//!   can tell "newer" from "concurrently edited" without asking anyone.
//! - [`keychain`] — the bridge: the file carries `NAME`, this machine supplies
//!   the value at spawn, and a machine that has the file but not the variable
//!   says so by name (T-0075 measured that the alternative is a live provider
//!   401 that mentions nothing).
//! - [`merge`] — keep-both conflicts named `<name>.conflict-<machine>-<ts>.<ext>`,
//!   plus the three hazards T-0075 verified (sibling extension, JSONC comments,
//!   array keys as sets).
//! - `engine` — the flow over the machine's own store, behind the `sqlite`
//!   feature because the vectors and the history live in the store the daemon
//!   already owns (`<socket>.db`).
//!
//! ## What is deliberately absent
//!
//! No transport code: T-0086's `Message::Sync` carries [`engine::SyncPayload`]
//! to the peer's daemon, which runs [`engine::SyncEngine::receive`] — the same
//! door a payload file goes through. No folder mode: §3.8 rejects it, because a
//! surprise overwrite of a whole tree is not an opt-in per file. No secret in a
//! synced file, ever, encrypted or otherwise — the key would travel with the
//! ciphertext.

pub mod keychain;
pub mod merge;
pub mod paths;
pub mod presets;
pub mod vectors;

#[cfg(feature = "sqlite")]
pub mod engine;

/// The worked case's file, as a string both the tests and the CLI's examples can
/// quote: one custom provider whose key is a reference.
pub const EXAMPLE_PROVIDER: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "verboo": {
      "name": "Verboo Code",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://code.verboo.ai/router/v1",
        "apiKey": "{env:VBK_PROD_KEY}"
      },
      "models": {
        "deepseek-v4-flash-0731": {
          "id": "deepseek-v4-flash-0731",
          "name": "Verboo deepseek-v4-flash-0731",
          "tool_call": true,
          "interleaved": "reasoning_content",
          "modalities": { "input": ["text"], "output": ["text"] },
          "limit": { "context": 1048576, "output": 65536 }
        }
      }
    }
  }
}
"#;
