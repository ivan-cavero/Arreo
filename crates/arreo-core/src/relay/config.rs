//! The arreo config file, section by section (T-0044, T-0076).
//!
//! One sentence: `--config PATH` (or `$ARREO_CONFIG`) names **one** TOML file
//! for this machine, `[relay]` is the daemon's section of it and `[tui]` is the
//! TUI's — and this module is the single place that knows the file's shape, so
//! no second crate grows a second parser for it.
//!
//! **Why this lives in core rather than in the daemon.** The daemon reads it to
//! join the account; the CLI reads it to read the account's directory; the TUI
//! reads its own section. The dependency rule forbids `arreo-cli` depending on
//! `arreo-server` (AGENTS.md), so a second copy of this parser in a client is
//! the only alternative — and a second copy of a validated config shape is two
//! answers to "what is a valid `[relay]` section", one of which will be wrong.
//! One owner, one answer.
//!
//! The module is named for its first section rather than for the file: moving
//! it to `arreo_core::config` would be a rename through `arreo-server`,
//! `arreo-cli` and `arreo-core::mesh`, which buys nothing but a nicer path.

use crate::identity::DeviceId;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;

/// The `[relay]` section of a configuration file.
#[derive(Debug, Clone, Deserialize)]
struct RelaySection {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    account: Option<String>,
    /// The device to open a session to at boot. Optional: a machine that only
    /// ever serves its peers needs none.
    #[serde(default)]
    peer: Option<String>,
    /// The name this machine claims in the account's directory (T-0056).
    /// Optional: without it the machine uses its hostname, which is what an
    /// operator would have typed.
    #[serde(default)]
    name: Option<String>,
}

/// The `[tui]` section of a configuration file (T-0076).
///
/// The TUI's corner of the same file: `reduce_motion` turns the attention
/// pulse into a still cue, and `exit_kills_daemon` opts the TUI into
/// drain-stopping the local daemon when it quits (T-0073). A key the TUI does
/// not know is ignored like any other unknown key — the file is shared, and
/// refusing to start over a typo in someone else's section would be the wrong
/// trade.
#[derive(Debug, Clone, Deserialize)]
struct TuiSection {
    #[serde(default)]
    reduce_motion: Option<bool>,
    #[serde(default)]
    exit_kills_daemon: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    relay: Option<RelaySection>,
    #[serde(default)]
    tui: Option<TuiSection>,
    #[serde(default)]
    worktree: Option<WorktreeSection>,
}

/// The `[worktree]` section of a configuration file (T-0091).
///
/// Where worktree-per-task checkouts live, and which repository they come from.
/// Both are optional: a machine that never passes `--worktree` reads neither, and
/// the defaults (the state directory, and the daemon's own working directory) are
/// what an operator who started the daemon in the repository they serve would
/// have typed anyway.
#[derive(Debug, Clone, Deserialize)]
struct WorktreeSection {
    /// The directory worktrees are made under. Default:
    /// `<state>/worktrees` — see [`crate::worktree::default_root`].
    #[serde(default)]
    root: Option<String>,
    /// The repository worktrees are made from. Default: the daemon's working
    /// directory.
    #[serde(default)]
    repo: Option<String>,
}

/// The `[worktree]` section, resolved: what the file asks for, with `None` where
/// it asks for nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeSettings {
    /// `Some(path)` = worktrees live under this directory.
    pub root: Option<String>,
    /// `Some(path)` = worktrees come from this repository.
    pub repo: Option<String>,
}

impl WorktreeSettings {
    /// Read the `[worktree]` section, if there is one.
    ///
    /// Same contract as [`TuiSettings::load`]: a missing file or a missing
    /// section is the default, and a file that does not parse is an error rather
    /// than a silent no-op — the user named this file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                })
            }
        };
        let parsed: ConfigFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        Ok(Self {
            root: parsed
                .worktree
                .as_ref()
                .and_then(|section| section.root.clone())
                .filter(|root| !root.trim().is_empty()),
            repo: parsed
                .worktree
                .as_ref()
                .and_then(|section| section.repo.clone())
                .filter(|repo| !repo.trim().is_empty()),
        })
    }
}

/// The `[tui]` section, resolved: what the file asks for, with `None` where it
/// asks for nothing (the TUI's own environment may still imply an answer — see
/// `arreo_tui::settings`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuiSettings {
    /// `Some(true)` = no blinking or pulsing, `Some(false)` = the file asked
    /// for motion, `None` = the file does not say.
    pub reduce_motion: Option<bool>,
    /// `Some(true)` = quitting this TUI should also drain-stop the *local*
    /// daemon (T-0073), `Some(false)` = the file asked for the opposite,
    /// `None` = the file does not say. `None` is not `false`: the TUI's
    /// `--shutdown-on-exit`/`--no-shutdown-on-exit` flags are the run's word,
    /// and an unset key must leave them the only answer.
    pub exit_kills_daemon: Option<bool>,
}

impl TuiSettings {
    /// Read the `[tui]` section, if there is one.
    ///
    /// A missing file or a file with no `[tui]` section is `Ok(default)` — most
    /// people never write one, and the config file exists for the daemon first.
    /// A file that does not parse, or a key of the wrong type, is an error the
    /// caller can put on the status line: the user named this file, so silently
    /// ignoring it is the invisible bug this repo keeps finding.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                })
            }
        };
        let parsed: ConfigFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        Ok(Self {
            reduce_motion: parsed.tui.as_ref().and_then(|tui| tui.reduce_motion),
            exit_kills_daemon: parsed.tui.as_ref().and_then(|tui| tui.exit_kills_daemon),
        })
    }
}

/// A validated relay configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySettings {
    pub addr: SocketAddr,
    pub account: String,
    pub peer: Option<DeviceId>,
    /// The name this machine asserts in the account's directory (T-0056). The
    /// relay may grant the deterministic suffix instead, and the daemon logs
    /// what it was actually granted.
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("cannot parse {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("the [relay] section of {path} is incomplete: {detail}")]
    Incomplete { path: String, detail: String },
}

/// Load the relay configuration, if the file enables it.
///
/// Returns `Ok(None)` for a missing file, a file with no `[relay]` section, or
/// `enabled = false` — all three mean "no relay", which is the default posture
/// and must cost nothing. A file that *does* enable the relay but is incomplete
/// is an error rather than a silent no-op: an operator who asked for the remote
/// path and quietly did not get it has a bug they cannot see.
pub fn load_config(path: &std::path::Path) -> Result<Option<RelaySettings>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.display().to_string(),
                detail: e.to_string(),
            })
        }
    };
    let parsed: ConfigFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let Some(section) = parsed.relay else {
        return Ok(None);
    };
    if !section.enabled {
        return Ok(None);
    }
    let incomplete = |detail: &str| ConfigError::Incomplete {
        path: path.display().to_string(),
        detail: detail.to_string(),
    };
    let addr = section
        .addr
        .as_deref()
        .ok_or_else(|| incomplete("[relay] enabled without `addr`"))?
        .parse::<SocketAddr>()
        .map_err(|e| incomplete(&format!("`addr` is not an IP:PORT address: {e}")))?;
    let account = section
        .account
        .filter(|account| !account.trim().is_empty())
        .ok_or_else(|| incomplete("[relay] enabled without `account`"))?;
    let peer = match section.peer.as_deref() {
        None => None,
        Some(raw) => Some(
            DeviceId::parse(raw)
                .map_err(|e| incomplete(&format!("`peer` is not a device id: {e}")))?,
        ),
    };
    let name = section
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(crate::mesh::default_machine_name);
    Ok(Some(RelaySettings {
        addr,
        account,
        peer,
        name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(text: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "arreo-relay-config-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, text).expect("write config");
        path
    }

    /// Missing file, no section, disabled: all three mean "no relay", which is
    /// the default posture and must cost nothing.
    #[test]
    fn no_relay_means_none_however_it_is_said() {
        let missing = std::env::temp_dir().join("arreo-relay-config-does-not-exist.toml");
        assert!(load_config(&missing)
            .expect("missing file is not an error")
            .is_none());

        let empty = write("[server]\nsocket = \"/tmp/x\"\n");
        assert!(load_config(&empty).expect("no section").is_none());

        let off = write("[relay]\nenabled = false\naddr = \"not an address\"\n");
        assert!(
            load_config(&off)
                .expect("disabled is not an error")
                .is_none(),
            "a disabled section must not even be validated"
        );
        let _ = std::fs::remove_file(&empty);
        let _ = std::fs::remove_file(&off);
    }

    /// Enabled but incomplete is a loud error naming the field: an operator who
    /// asked for the remote path and quietly did not get it has an invisible bug.
    #[test]
    fn an_incomplete_section_names_what_is_missing() {
        let no_addr = write("[relay]\nenabled = true\naccount = \"acct-1\"\n");
        let error = load_config(&no_addr).expect_err("missing addr");
        assert!(error.to_string().contains("addr"), "{error}");

        let no_account = write("[relay]\nenabled = true\naddr = \"127.0.0.1:1\"\n");
        let error = load_config(&no_account).expect_err("missing account");
        assert!(error.to_string().contains("account"), "{error}");

        let bad_addr =
            write("[relay]\nenabled = true\naddr = \"relay.example.com\"\naccount = \"acct-1\"\n");
        let error = load_config(&bad_addr).expect_err("hostname is not an address");
        assert!(error.to_string().contains("IP:PORT"), "{error}");

        let bad_peer = write(
            "[relay]\nenabled = true\naddr = \"127.0.0.1:1\"\naccount = \"acct-1\"\npeer = \"nope\"\n",
        );
        assert!(load_config(&bad_peer)
            .expect_err("bad peer")
            .to_string()
            .contains("peer"));

        for path in [no_addr, no_account, bad_addr, bad_peer] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// A complete section loads, the name defaults to this machine's, and an
    /// explicit name wins.
    #[test]
    fn a_complete_section_loads_with_a_name() {
        let path = write(
            "[relay]\nenabled = true\naddr = \"127.0.0.1:5000\"\naccount = \"acct-1\"\nname = \"workbox\"\n",
        );
        let settings = load_config(&path).expect("ok").expect("some");
        assert_eq!(settings.account, "acct-1");
        assert_eq!(settings.name, "workbox");
        let _ = std::fs::remove_file(&path);

        let unnamed =
            write("[relay]\nenabled = true\naddr = \"127.0.0.1:5000\"\naccount = \"acct-1\"\n");
        let settings = load_config(&unnamed).expect("ok").expect("some");
        assert_eq!(
            settings.name,
            crate::mesh::default_machine_name(),
            "no name configured means this machine's hostname"
        );
        let _ = std::fs::remove_file(&unnamed);
    }

    /// The `[tui]` section (T-0076): one file, two sections, and neither
    /// section's keys are the other's business.
    #[test]
    fn the_tui_section_is_read_from_the_same_file() {
        // Nothing to say: a missing file and a file without the section both
        // mean "the file does not ask for anything".
        let missing = std::env::temp_dir().join("arreo-tui-config-does-not-exist.toml");
        assert_eq!(
            TuiSettings::load(&missing).expect("missing file is not an error"),
            TuiSettings::default()
        );
        let relay_only =
            write("[relay]\nenabled = true\naddr = \"127.0.0.1:1\"\naccount = \"a\"\n");
        assert_eq!(
            TuiSettings::load(&relay_only).expect("no [tui] section"),
            TuiSettings::default()
        );
        let _ = std::fs::remove_file(&relay_only);

        let both = write("[relay]\nenabled = false\n\n[tui]\nreduce_motion = true\n");
        assert_eq!(
            TuiSettings::load(&both).expect("ok").reduce_motion,
            Some(true),
            "the [tui] section is read beside the [relay] one"
        );
        // The daemon's view of the same file is unaffected by the new section.
        assert!(load_config(&both)
            .expect("relay side still parses")
            .is_none());
        let _ = std::fs::remove_file(&both);

        // An explicit false is a value, not an absence: the caller may still
        // overrule it for its own reasons, but it must not be lost here.
        let off = write("[tui]\nreduce_motion = false\n");
        assert_eq!(
            TuiSettings::load(&off).expect("ok").reduce_motion,
            Some(false)
        );
        let _ = std::fs::remove_file(&off);
    }

    /// `tui.exit_kills_daemon` (T-0073): the opt-in to stopping the local
    /// daemon when the TUI quits. An unset key must stay `None` — the flag is
    /// the run's word, and a default of `false` here would be a second answer
    /// to "did anyone ask for this".
    #[test]
    fn the_exit_kills_daemon_key_is_read_beside_reduce_motion() {
        let missing = std::env::temp_dir().join("arreo-tui-config-does-not-exist-73.toml");
        assert_eq!(
            TuiSettings::load(&missing).expect("missing file is not an error"),
            TuiSettings::default()
        );

        let on = write("[tui]\nexit_kills_daemon = true\n");
        let settings = TuiSettings::load(&on).expect("ok");
        assert_eq!(settings.exit_kills_daemon, Some(true));
        assert_eq!(settings.reduce_motion, None, "one key is not the other");
        let _ = std::fs::remove_file(&on);

        // An explicit false is a value, not an absence: the TUI's flag may
        // still overrule it, but the file's word must not be lost here.
        let off = write("[tui]\nexit_kills_daemon = false\nreduce_motion = true\n");
        let settings = TuiSettings::load(&off).expect("ok");
        assert_eq!(settings.exit_kills_daemon, Some(false));
        assert_eq!(settings.reduce_motion, Some(true), "both keys, one file");
        let _ = std::fs::remove_file(&off);

        // A wrong type is the caller's to report, the same contract
        // `reduce_motion` set — and the reason names the key.
        let wrong_type = write("[tui]\nexit_kills_daemon = \"yes\"\n");
        let error = TuiSettings::load(&wrong_type).expect_err("wrong type");
        assert!(
            error.to_string().contains("exit_kills_daemon"),
            "the reason must name the key: {error}"
        );
        let _ = std::fs::remove_file(&wrong_type);
    }

    /// A file the user named that does not parse, or that types a key wrongly,
    /// is an error with a reason — not a silent default.
    #[test]
    fn a_broken_tui_section_is_reported() {
        let malformed = write("[tui\nreduce_motion = ");
        let error = TuiSettings::load(&malformed).expect_err("malformed");
        assert!(error.to_string().contains("cannot parse"), "{error}");

        let wrong_type = write("[tui]\nreduce_motion = \"yes\"\n");
        let error = TuiSettings::load(&wrong_type).expect_err("wrong type");
        assert!(
            error.to_string().contains("reduce_motion"),
            "the reason must name the key: {error}"
        );

        // Unknown keys are ignored, both ours and the daemon's.
        let extra = write("[tui]\nreduce_motion = true\nfuture_key = 3\n\n[other]\nx = 1\n");
        assert_eq!(
            TuiSettings::load(&extra)
                .expect("unknown keys are ignored")
                .reduce_motion,
            Some(true)
        );

        for path in [malformed, wrong_type, extra] {
            let _ = std::fs::remove_file(path);
        }
    }
}
