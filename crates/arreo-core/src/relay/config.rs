//! The `[relay]` configuration, shared by the daemon and the CLI (T-0044).
//!
//! **Why this lives in core rather than in the daemon.** The daemon reads it to
//! join the account; the CLI reads it to read the account's directory. The
//! dependency rule forbids `arreo-cli` depending on `arreo-server` (AGENTS.md),
//! so a second copy of this parser in the CLI is the only alternative — and a
//! second copy of a validated config shape is two answers to "what is a valid
//! `[relay]` section", one of which will be wrong. One owner, one answer.

use crate::identity::DeviceId;
use serde::Deserialize;
use std::net::SocketAddr;

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

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    relay: Option<RelaySection>,
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
}
