//! Symbolic path resolution (T-0083): `$XDG_CONFIG_HOME/opencode/opencode.jsonc`
//! meant the same thing on four machines with four different homes.
//!
//! One sentence: a preset stores the *symbolic* location and this module turns
//! it into a real path using the receiving machine's own roots, so the file's
//! bytes are identical everywhere while its location is not.
//!
//! Why not store the resolved path: ROADMAP §3.11 gives each OS a different
//! config root (`$XDG_CONFIG_HOME` on Linux, `~/Library/Application Support` on
//! macOS, `%APPDATA%` on Windows), and §3.8's whole case is "add a provider once,
//! on every machine" — a preset carrying `/home/dev/.config/...` would replicate
//! one machine's layout to the others.
//!
//! **Nothing is guessed.** A symbolic root that this machine does not set is a
//! refusal that names the variable, not a fallback into a plausible-looking
//! directory: writing an opencode config into an invented path would look like a
//! successful sync and change nothing. The XDG defaults below are the
//! specification's own fallbacks and are the only derivations here; pi's and
//! omp's agent root has no verified default (T-0075's probes always set
//! `PI_CODING_AGENT_DIR`), so it is required.

use std::path::PathBuf;

/// The OS whose path conventions apply. Passed explicitly rather than read from
/// `cfg!` so the macOS and Windows layouts are testable on the Linux box that
/// runs the tests — the expansion is data, not a compile-time assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    /// The OS this binary was built for.
    #[must_use]
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::MacOs => "macos",
            Os::Windows => "windows",
        }
    }
}

/// Why a symbolic path could not be resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path names a root this machine does not have set. The token is the
    /// variable's spelling, so the message tells the operator what to export.
    Unset { token: String },
    /// The path is not symbolic at all — a PROJECT file, or a registry entry
    /// written as a literal path. Syncing a literal path is syncing one
    /// machine's layout, so it is refused rather than half-supported.
    NotSymbolic { path: String },
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Unset { token } => write!(
                f,
                "{token} is not set on this machine, so the preset's path cannot be resolved \
                 (export it, or point the file at a path of your own)"
            ),
            PathError::NotSymbolic { path } => {
                write!(f, "{path} is not a symbolic path")
            }
        }
    }
}

impl std::error::Error for PathError {}

/// One machine's idea of where things live.
///
/// Constructed from the process environment for real use
/// ([`MachineEnv::from_process`]) and field by field for tests and the two-root
/// slice, because "which machine is this" must be data the caller supplies —
/// otherwise two machines in one process would be impossible to express.
#[derive(Debug, Clone)]
pub struct MachineEnv {
    name: String,
    os: Os,
    home: PathBuf,
    xdg_config: Option<PathBuf>,
    xdg_data: Option<PathBuf>,
    xdg_state: Option<PathBuf>,
    xdg_cache: Option<PathBuf>,
    pi_agent_dir: Option<PathBuf>,
    appdata: Option<PathBuf>,
}

impl MachineEnv {
    /// A machine with only a home, for tests and for a caller that resolves
    /// nothing OS-specific.
    #[must_use]
    pub fn new(name: &str, os: Os, home: PathBuf) -> Self {
        Self {
            name: name.to_string(),
            os,
            home,
            xdg_config: None,
            xdg_data: None,
            xdg_state: None,
            xdg_cache: None,
            pi_agent_dir: None,
            appdata: None,
        }
    }

    /// This process's machine: the environment the daemon or CLI was started in.
    #[must_use]
    pub fn from_process(name: &str) -> Self {
        // **An unset or empty HOME is absent, not `.`** (review F8). Falling back
        // to the current directory made every XDG fallback relative, so a receive
        // on a machine without HOME would create and write a harness config under
        // whatever directory the process happened to start in — silently, and
        // reporting success. The module's own promise is that a root this machine
        // does not set is a refusal that names the variable, so an empty value is
        // treated exactly like a missing one (the same rule `env_path` applies to
        // the XDG variables).
        let home = env_path("HOME").unwrap_or_else(|| PathBuf::from(""));
        Self {
            name: name.to_string(),
            os: Os::host(),
            home,
            xdg_config: env_path("XDG_CONFIG_HOME"),
            xdg_data: env_path("XDG_DATA_HOME"),
            xdg_state: env_path("XDG_STATE_HOME"),
            xdg_cache: env_path("XDG_CACHE_HOME"),
            pi_agent_dir: env_path("PI_CODING_AGENT_DIR"),
            appdata: env_path("APPDATA"),
        }
    }

    #[must_use]
    pub fn with_config_home(mut self, path: PathBuf) -> Self {
        self.xdg_config = Some(path);
        self
    }

    #[must_use]
    pub fn with_data_home(mut self, path: PathBuf) -> Self {
        self.xdg_data = Some(path);
        self
    }

    #[must_use]
    pub fn with_state_home(mut self, path: PathBuf) -> Self {
        self.xdg_state = Some(path);
        self
    }

    #[must_use]
    pub fn with_cache_home(mut self, path: PathBuf) -> Self {
        self.xdg_cache = Some(path);
        self
    }

    #[must_use]
    pub fn with_pi_agent_dir(mut self, path: PathBuf) -> Self {
        self.pi_agent_dir = Some(path);
        self
    }

    #[must_use]
    pub fn with_appdata(mut self, path: PathBuf) -> Self {
        self.appdata = Some(path);
        self
    }

    /// This machine's name: the id a version vector counts under and the name a
    /// conflict copy carries.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Resolve a symbolic path against this machine's roots.
    ///
    /// The grammar is the one the registry uses: a leading `$TOKEN` or
    /// `%TOKEN%`, then literal components separated by `/`. Components are
    /// joined with [`Path::join`] rather than by string replacement so a
    /// Windows machine gets backslashes while the preset keeps one spelling.
    pub fn resolve(&self, symbolic: &str) -> Result<PathBuf, PathError> {
        let (root, token, rest) = if let Some(rest) = symbolic.strip_prefix('$') {
            let end = rest.find('/').unwrap_or(rest.len());
            (&rest[..end], format!("${}", &rest[..end]), &rest[end..])
        } else if let Some(rest) = symbolic.strip_prefix('%') {
            let end = rest.find('%').ok_or_else(|| PathError::NotSymbolic {
                path: symbolic.to_string(),
            })?;
            (
                &rest[..end],
                format!("%{}%", &rest[..end]),
                &rest[end + 1..],
            )
        } else {
            return Err(PathError::NotSymbolic {
                path: symbolic.to_string(),
            });
        };
        let mut path = self.root(root, &token)?;
        for component in rest.split('/').filter(|c| !c.is_empty()) {
            path.push(component);
        }
        Ok(path)
    }

    /// The directory a token names on this machine.
    fn root(&self, token: &str, spelled: &str) -> Result<PathBuf, PathError> {
        let unset = || PathError::Unset {
            token: spelled.to_string(),
        };
        match token {
            "XDG_CONFIG_HOME" => self
                .xdg_config
                .clone()
                .or_else(|| self.platform_default("config"))
                .ok_or_else(unset),
            "XDG_DATA_HOME" => self
                .xdg_data
                .clone()
                .or_else(|| self.platform_default("data"))
                .ok_or_else(unset),
            "XDG_STATE_HOME" => self
                .xdg_state
                .clone()
                .or_else(|| self.platform_default("state"))
                .ok_or_else(unset),
            "XDG_CACHE_HOME" => self
                .xdg_cache
                .clone()
                .or_else(|| self.platform_default("cache"))
                .ok_or_else(unset),
            "HOME" => Ok(self.home.clone()),
            "APPDATA" => self.appdata.clone().ok_or_else(unset),
            // No verified default: T-0075's probes always set this, and pi's and
            // omp's own defaults differ (`~/.pi/agent` vs the measured
            // `$HOME/.omp/agent`), so inventing one would guess a location for
            // the file we are about to write.
            "PI_CODING_AGENT_DIR" => self.pi_agent_dir.clone().ok_or_else(unset),
            other => Err(PathError::Unset {
                token: format!("${other}"),
            }),
        }
    }

    /// The XDG defaults this OS defines when the variable is unset.
    ///
    /// Linux: the XDG basedir spec's fallbacks. macOS and Windows: ROADMAP
    /// §3.11's roots, from the survey — `~/Library/Application Support` and
    /// `%APPDATA%` — with the cache under `~/Library/Caches` on macOS, which is
    /// where that platform puts caches that are not backed up.
    fn platform_default(&self, kind: &str) -> Option<PathBuf> {
        // A machine with no HOME has no per-user default to fall back to: joining
        // onto an empty path would produce a *relative* root (`./.config`), and a
        // receive would then write a harness config under the process's working
        // directory. `None` makes `root()` return the refusal that names `$HOME`
        // (review F8).
        if self.home.as_os_str().is_empty() {
            return None;
        }
        match (self.os, kind) {
            (Os::Linux, "config") => Some(self.home.join(".config")),
            (Os::Linux, "data") => Some(self.home.join(".local/share")),
            (Os::Linux, "state") => Some(self.home.join(".local/state")),
            (Os::Linux, "cache") => Some(self.home.join(".cache")),
            (Os::MacOs, "cache") => Some(self.home.join("Library/Caches")),
            (Os::MacOs, _) => Some(self.home.join("Library/Application Support")),
            (Os::Windows, kind) => {
                let appdata = self.appdata.clone()?;
                // %APPDATA% is the roaming root ROADMAP §3.11 names; caches and
                // state are the machine-local analogue of it.
                if kind == "cache" || kind == "state" {
                    Some(appdata.join("Local"))
                } else {
                    Some(appdata)
                }
            }
            _ => None,
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSONC: &str = "$XDG_CONFIG_HOME/opencode/opencode.jsonc";

    #[test]
    fn one_symbolic_path_is_four_machines_paths() {
        let linux = MachineEnv::new("workbox", Os::Linux, PathBuf::from("/home/dev"))
            .with_config_home(PathBuf::from("/home/dev/.config"));
        assert_eq!(
            linux.resolve(JSONC).expect("resolved"),
            PathBuf::from("/home/dev/.config/opencode/opencode.jsonc")
        );

        // macOS has no XDG_CONFIG_HOME: the platform root answers.
        let mac = MachineEnv::new("mac", Os::MacOs, PathBuf::from("/Users/dev"));
        assert_eq!(
            mac.resolve(JSONC).expect("resolved"),
            PathBuf::from("/Users/dev/Library/Application Support/opencode/opencode.jsonc")
        );

        // Windows resolves the same component under %APPDATA%.
        let windows = MachineEnv::new("desk", Os::Windows, PathBuf::from("C:/Users/dev"))
            .with_appdata(PathBuf::from("C:/Users/dev/AppData/Roaming"));
        assert_eq!(
            windows.resolve(JSONC).expect("resolved"),
            PathBuf::from("C:/Users/dev/AppData/Roaming/opencode/opencode.jsonc")
        );
    }

    /// **A machine with no HOME has no per-user default** (review F8). The
    /// fallback used to be the current directory, which made every XDG root
    /// *relative*: a receive on such a machine would create and write a harness
    /// config under whatever directory the process was started in — silently, and
    /// reporting success. The module's promise is a refusal that names the
    /// variable, so an empty HOME must produce one.
    #[test]
    fn an_empty_home_is_a_refusal_not_a_relative_root() {
        let machine = MachineEnv::new("nohome", Os::Linux, PathBuf::from(""));
        let error = machine
            .resolve("$XDG_CONFIG_HOME/opencode/opencode.jsonc")
            .expect_err("no HOME, no default");
        assert_eq!(
            error,
            PathError::Unset {
                token: "$XDG_CONFIG_HOME".to_string()
            }
        );
        // And the resolved path can never be relative, whatever the machine.
        let with_home = MachineEnv::new("home", Os::Linux, PathBuf::from("/home/u"));
        let resolved = with_home
            .resolve("$XDG_CONFIG_HOME/opencode/opencode.jsonc")
            .expect("resolves");
        assert!(resolved.is_absolute(), "{resolved:?}");
    }

    #[test]
    fn an_unset_root_is_named_rather_than_invented() {
        let machine = MachineEnv::new("rpi5", Os::Linux, PathBuf::from("/home/pi"));
        let error = machine
            .resolve("$PI_CODING_AGENT_DIR/models.json")
            .expect_err("pi's root is required, not guessed");
        assert_eq!(
            error,
            PathError::Unset {
                token: "$PI_CODING_AGENT_DIR".to_string()
            }
        );
        assert!(error.to_string().contains("PI_CODING_AGENT_DIR"));

        // A set root resolves, so the refusal above is about the variable and
        // not about the token being unsupported.
        let set = machine.with_pi_agent_dir(PathBuf::from("/home/pi/.pi/agent"));
        assert_eq!(
            set.resolve("$PI_CODING_AGENT_DIR/models.json")
                .expect("resolved"),
            PathBuf::from("/home/pi/.pi/agent/models.json")
        );
    }

    #[test]
    fn the_linux_xdg_fallbacks_are_the_specs_own() {
        let machine = MachineEnv::new("rpi5", Os::Linux, PathBuf::from("/home/pi"));
        assert_eq!(
            machine
                .resolve("$XDG_DATA_HOME/opencode/opencode.db")
                .expect("resolved"),
            PathBuf::from("/home/pi/.local/share/opencode/opencode.db")
        );
        assert_eq!(
            machine
                .resolve("$XDG_CACHE_HOME/opencode")
                .expect("resolved"),
            PathBuf::from("/home/pi/.cache/opencode")
        );
    }

    #[test]
    fn a_literal_path_is_refused_rather_than_synced() {
        let machine = MachineEnv::new("workbox", Os::Linux, PathBuf::from("/home/dev"));
        assert_eq!(
            machine.resolve("/home/dev/.config/opencode/opencode.jsonc"),
            Err(PathError::NotSymbolic {
                path: "/home/dev/.config/opencode/opencode.jsonc".to_string()
            })
        );
    }

    #[test]
    fn the_registry_all_resolves_on_a_fully_specified_machine() {
        let machine = MachineEnv::new("workbox", Os::Linux, PathBuf::from("/home/dev"))
            .with_pi_agent_dir(PathBuf::from("/home/dev/.pi/agent"));
        for (preset, entry) in super::super::presets::files() {
            if entry.class == super::super::presets::Class::Sync {
                let path = machine
                    .resolve(entry.path)
                    .unwrap_or_else(|e| panic!("{}/{}: {e}", preset.harness.id(), entry.name));
                assert!(path.is_absolute(), "{}", entry.name);
            }
        }
    }
}
