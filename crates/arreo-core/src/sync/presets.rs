//! The preset registry (T-0083): what may leave a machine, file by file.
//!
//! One sentence: a preset says, per harness, which files are portable intent
//! (SYNC), which are machine-local state that must never cross (LOCAL), and
//! which belong to a repository rather than to the mesh (PROJECT) — and it says
//! where each one lives in a form every machine can resolve for itself.
//!
//! The data is `docs/harness-centralization.md` §2 (T-0075's survey of the live
//! CLIs, rows traced to transcripts), not a guess: only opencode, pi and omp
//! have entries, because those are the three whose paths and dialects were
//! recorded. A preset for a harness nobody measured would be a guess wearing a
//! schema, so an unrecorded harness has none and its files fall back to a
//! user-declared path where the secret scan is the only protection.
//!
//! **Why the fence is per file and not per folder.** opencode keeps portable
//! intent (`opencode.jsonc`) and the 2.9 GB session database in two different
//! XDG roots, and pi interleaves `models.json` (portable) with `auth.json` and
//! `sessions/` (never) in the *same* directory. A folder rule would be wrong for
//! both, so the class is a property of the file and the registry is the only
//! place it is decided.
//!
//! **The class decides before the scan, never instead of it.** A LOCAL file is
//! refused because of what it *is*; the scan is the second net for a file that
//! was already allowed. A refusal that read "possible secret" for `auth.json`
//! would tell the operator their credentials file is dirty when the truth is
//! that it is not syncable at all.

use std::path::PathBuf;

/// Which of the three classes a file is in (`docs/harness-centralization.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Portable intent: may travel once the scan passes and no path is absolute.
    Sync,
    /// Credentials, session state, caches, logs, installs. Never travels.
    Local,
    /// Belongs to a repository and travels with git; syncing it would fight the repo.
    Project,
}

impl Class {
    /// The word the CLI prints and the JSON carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Sync => "sync",
            Class::Local => "local",
            Class::Project => "project",
        }
    }
}

/// The three harnesses whose config paths T-0075 recorded live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Opencode,
    Pi,
    Omp,
}

impl Harness {
    /// Every harness a preset exists for. Deliberately not "every harness
    /// Arreo knows": the adapters registry is larger than the surveyed set.
    pub const ALL: [Harness; 3] = [Harness::Opencode, Harness::Pi, Harness::Omp];

    /// The id the CLI prints and the JSON carries.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
            Harness::Omp => "omp",
        }
    }
}

/// How a harness spells "this value is the name of a variable, resolve it
/// yourself" (the dialect table of `docs/harness-centralization.md` §3.2).
///
/// The spellings are measured, not inferred, and two of them contradict the
/// survey's prose — see [`Dialect::OmpBareName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// opencode: `{env:NAME}`. Verified: with the variable unset the provider
    /// answers a live 401, so opencode genuinely reads the environment.
    OpencodeEnvBrace,
    /// pi: `$NAME` and `${NAME}` both work (both sent a working request).
    PiDollar,
    /// omp: the bare variable NAME, with **no sigil**.
    ///
    /// The survey's prose said `$NAME`; T-0080 measured the header omp actually
    /// sends through a TLS proxy and both sigil forms are forwarded verbatim as
    /// the bearer token (401), while the bare name is looked up in
    /// `process.env` and works. The transcript was right and the prose was
    /// wrong, so the preset follows the measurement (`.loop/evidence/T-0080/`
    /// `omp-apikey-env-syntax.txt`). Config sync for omp therefore writes the
    /// variable name and nothing else.
    OmpBareName,
}

impl Dialect {
    /// The reference to `name` as this machine's harness spells it: the form
    /// that lands on disk when a payload written in the neutral Arreo form
    /// (`${ARREO_ENV:NAME}`) arrives here.
    #[must_use]
    pub fn native(self, name: &str) -> String {
        match self {
            Dialect::OpencodeEnvBrace => format!("{{env:{name}}}"),
            Dialect::PiDollar => format!("${{{name}}}"),
            Dialect::OmpBareName => name.to_string(),
        }
    }

    /// The word the CLI prints and the JSON carries.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Dialect::OpencodeEnvBrace => "opencode:{env:NAME}",
            Dialect::PiDollar => "pi:$NAME|${NAME}",
            Dialect::OmpBareName => "omp:NAME",
        }
    }
}

/// One file a preset knows about.
#[derive(Debug, Clone, Copy)]
pub struct PresetFile {
    /// The logical name: the key version vectors, history and
    /// `arreo sync revert <file>` use. Stable across machines — the whole point
    /// of the feature is that two machines agree on what "opencode.jsonc" is.
    pub name: &'static str,
    /// Which class the file is in. Decided here and nowhere else.
    pub class: Class,
    /// The path in **symbolic** form (`$XDG_CONFIG_HOME/…`, `%APPDATA%`, …), so
    /// a preset stores the location and the receiving machine resolves it
    /// (ROADMAP §3.11). Empty for PROJECT files, which have no machine path.
    pub path: &'static str,
    /// The fields that carry intent. Printed by `arreo sync list --json`;
    /// the *enforced* half of the field rules is
    /// [`PresetFile::local_fields`], because a whitelist strict enough to be
    /// enforced would have to know each harness's real schema and this survey
    /// does not (it records what is portable, not what exists).
    pub portable: &'static [&'static str],
    /// Fields that are machine-local *inside* an otherwise syncable file: a
    /// literal in one of these never travels, so a synced file that carries one
    /// is refused by name rather than silently overwriting the peer's value.
    pub local_fields: &'static [&'static str],
    /// Keys that are sets rather than scalars: a merge unions them.
    pub union_arrays: &'static [&'static str],
    /// Does this harness merge a sibling extension in the same directory?
    /// Verified for opencode: with both `opencode.json` and `opencode.jsonc`
    /// present, `opencode debug config` listed the providers from *both*. The
    /// consequence is in [`sibling_of`].
    pub sibling_merges: bool,
}

/// One harness and the files it owns.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub harness: Harness,
    pub dialect: Dialect,
    pub files: &'static [PresetFile],
}

const OPENCODE_PROVIDER_FIELDS: &[&str] = &[
    "provider.<id>.name",
    "provider.<id>.npm",
    "provider.<id>.options.baseURL",
    "provider.<id>.models.*",
    "plugin",
    "agent",
    "mcp",
];

const OPENCODE: Preset = Preset {
    harness: Harness::Opencode,
    dialect: Dialect::OpencodeEnvBrace,
    files: &[
        PresetFile {
            name: "opencode.jsonc",
            class: Class::Sync,
            path: "$XDG_CONFIG_HOME/opencode/opencode.jsonc",
            portable: OPENCODE_PROVIDER_FIELDS,
            local_fields: &["apiKey"],
            union_arrays: &["plugin"],
            sibling_merges: true,
        },
        PresetFile {
            name: "tui.jsonc",
            class: Class::Sync,
            path: "$XDG_CONFIG_HOME/opencode/tui.jsonc",
            portable: &["theme", "plugin"],
            local_fields: &[],
            union_arrays: &["plugin"],
            sibling_merges: true,
        },
        // The sibling itself. Never syncable, and registered so that naming it
        // is a class refusal ("this file is not syncable") instead of a custom
        // path that would happily replicate half of opencode's live config.
        PresetFile {
            name: "opencode.json",
            class: Class::Local,
            path: "$XDG_CONFIG_HOME/opencode/opencode.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: true,
        },
        // Reproducible from `plugin`; per-machine install artefacts. The real
        // directory carries a checked-in `.gitignore` with exactly these names.
        PresetFile {
            name: "node_modules",
            class: Class::Local,
            path: "$XDG_CONFIG_HOME/opencode/node_modules",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "package.json",
            class: Class::Local,
            path: "$XDG_CONFIG_HOME/opencode/package.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "package-lock.json",
            class: Class::Local,
            path: "$XDG_CONFIG_HOME/opencode/package-lock.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "bun.lock",
            class: Class::Local,
            path: "$XDG_CONFIG_HOME/opencode/bun.lock",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        // Sessions, messages, parts AND the `credential`/`account` rows live in
        // this one SQLite file. Never sync: it is state, it is huge, and half of
        // it is secrets.
        PresetFile {
            name: "opencode.db",
            class: Class::Local,
            path: "$XDG_DATA_HOME/opencode/opencode.db",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "opencode-store",
            class: Class::Local,
            path: "$XDG_DATA_HOME/opencode/storage",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "opencode-logs",
            class: Class::Local,
            path: "$XDG_DATA_HOME/opencode/log",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "opencode-cache",
            class: Class::Local,
            path: "$XDG_CACHE_HOME/opencode",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "opencode-state",
            class: Class::Local,
            path: "$XDG_STATE_HOME/opencode",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
    ],
};

/// pi interleaves portable intent with credentials in one directory, which is
/// the case the per-file fence exists for.
const PI: Preset = Preset {
    harness: Harness::Pi,
    dialect: Dialect::PiDollar,
    files: &[
        PresetFile {
            name: "models.json",
            class: Class::Sync,
            path: "$PI_CODING_AGENT_DIR/models.json",
            portable: &[
                "providers.<id>.baseUrl",
                "providers.<id>.api",
                "providers.<id>.authHeader",
                "providers.<id>.models[]",
            ],
            local_fields: &["apiKey"],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "settings.json",
            class: Class::Sync,
            path: "$PI_CODING_AGENT_DIR/settings.json",
            portable: &["theme", "packages", "resource toggles"],
            // The changelog marker is per-machine noise: it changes when this
            // machine updates pi, so letting it travel would have every sync
            // rewrite it and every machine fight over the same key.
            local_fields: &["lastChangelogVersion"],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "auth.json",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/auth.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-sessions",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/sessions",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-npm",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/npm",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-bin",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/bin",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "subagents.json",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/subagents.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "models-store.json",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/models-store.json",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-extensions",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/extensions",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-agents",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/agents",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-chains",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/chains",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "pi-intercom",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/intercom",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
    ],
};

const OMP: Preset = Preset {
    harness: Harness::Omp,
    dialect: Dialect::OmpBareName,
    files: &[
        PresetFile {
            name: "models.yml",
            class: Class::Sync,
            path: "$PI_CODING_AGENT_DIR/models.yml",
            portable: &[
                "providers.<id>.name",
                "providers.<id>.baseUrl",
                "providers.<id>.api",
                "providers.<id>.models[]",
                "providers.<id>.compat.thinkingFormat",
                "providers.<id>.compat.reasoningContentField",
            ],
            local_fields: &["apiKey"],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "config.yml",
            class: Class::Sync,
            path: "$PI_CODING_AGENT_DIR/config.yml",
            portable: &["modelRoles.default", "theme.dark", "composer.shape"],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "agent.db",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/agent.db",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "history.db",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/history.db",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-sessions",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/sessions",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-terminal-sessions",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/terminal-sessions",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-logs",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/logs",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-cache",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/cache",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-run",
            class: Class::Local,
            path: "$PI_CODING_AGENT_DIR/run",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        // `omp config init-xdg` creates these; `omp config path` still reports
        // `$HOME/.omp/agent`, so both roots exist on a machine that ran it.
        PresetFile {
            name: "omp-xdg-data",
            class: Class::Local,
            path: "$XDG_DATA_HOME/omp",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-xdg-state",
            class: Class::Local,
            path: "$XDG_STATE_HOME/omp",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
        PresetFile {
            name: "omp-xdg-cache",
            class: Class::Local,
            path: "$XDG_CACHE_HOME/omp",
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        },
    ],
};

/// The registry: three harnesses, because three were surveyed.
pub const PRESETS: [Preset; 3] = [OPENCODE, PI, OMP];

/// Every preset.
#[must_use]
pub fn presets() -> &'static [Preset] {
    &PRESETS
}

/// The preset a logical file name belongs to.
#[must_use]
pub fn file(name: &str) -> Option<(&'static Preset, &'static PresetFile)> {
    for preset in presets() {
        for entry in preset.files {
            if entry.name == name {
                return Some((preset, entry));
            }
        }
    }
    None
}

/// Every file in the registry, in registry order.
pub fn files() -> impl Iterator<Item = (&'static Preset, &'static PresetFile)> {
    presets()
        .iter()
        .flat_map(|preset| preset.files.iter().map(move |entry| (preset, entry)))
}

/// The harness's dialect, or `None` for a harness with no preset.
#[must_use]
pub fn dialect_for(harness: &str) -> Option<Dialect> {
    presets()
        .iter()
        .find(|preset| preset.harness.id() == harness)
        .map(|preset| preset.dialect)
}

/// The file a harness merges alongside `entry`, when it merges two extensions
/// in the same directory.
///
/// Verified for opencode: with `opencode.json` and `opencode.jsonc` both
/// present, `debug config` listed the providers of both. The consequence the
/// sync cares about is that writing one of them while the sibling exists would
/// put two provider lists live at once, so [`crate::sync::engine`] refuses
/// instead of silently diverging, and `arreo sync merge` reconciles.
#[must_use]
pub fn sibling_of(entry: &PresetFile, resolved: &std::path::Path) -> Option<PathBuf> {
    sibling_path_for(entry.sibling_merges, resolved)
}

/// The sibling rule itself, for a caller that has the flag rather than the
/// registry entry (the engine resolves a file once and then works from that).
#[must_use]
pub fn sibling_path_for(merges: bool, resolved: &std::path::Path) -> Option<PathBuf> {
    if !merges {
        return None;
    }
    let extension = resolved.extension()?.to_str()?;
    let other = match extension {
        "jsonc" => "json",
        "json" => "jsonc",
        _ => return None,
    };
    let mut sibling = resolved.to_path_buf();
    sibling.set_extension(other);
    Some(sibling)
}

/// A rule from the LOCAL deny-list that a user-declared path tripped.
///
/// The list exists for the files no preset knows: a harness nobody recorded, or
/// an operator pointing `arreo sync push` at a path of their own. The preset
/// table is exact data for three harnesses; this is the floor under everything
/// else, and it is checked **before** the scan because "this file is state, not
/// intent" is a different answer from "this file is dirty".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyRule {
    /// A name that is a credential store wherever it lives (`auth.json`,
    /// `credentials`, …).
    Credentials,
    /// A database or its write-ahead sidecars (`*.db`, `*.sqlite`, `-wal`,
    /// `-shm`): session state, and half of it is secrets.
    Database,
    /// A directory a path component names: `sessions`, `node_modules`, `cache`,
    /// `logs`, `blobs`, `run`.
    StateDirectory,
    /// A log or lock: noise that reproduces itself on the receiving machine.
    Noise,
    /// A private key material file (`*.pem`, `*.key`, `id_*`).
    KeyMaterial,
    /// Codex's per-hook trust hashes: they hash a *local* file, so they diverge
    /// on every machine and would push a hook-trust prompt on every sync.
    HookTrustHash,
}

impl DenyRule {
    /// The sentence a refusal prints, so the operator knows which rule fired.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DenyRule::Credentials => "credential store",
            DenyRule::Database => "database or database sidecar (session state)",
            DenyRule::StateDirectory => "state directory (sessions/caches/installs)",
            DenyRule::Noise => "log or lock (reproduces itself on the peer)",
            DenyRule::KeyMaterial => "key material",
            DenyRule::HookTrustHash => "per-machine hook trust hash",
        }
    }
}

/// The credential-store names, whatever else the path looks like.
const DENY_NAMES: [&str; 9] = [
    "auth.json",
    "credentials",
    "credentials.json",
    "secrets.json",
    "keychain.json",
    // Key material and host trust that is not `id_*` and not a `.pem`
    // (review F7): an ssh identity, its known hosts, and an env file, none of
    // which any harness config should ever carry.
    "authorized_keys",
    "known_hosts",
    ".env",
    "id_ed25519.pub",
];

/// Directory components that mark state rather than intent.
const DENY_DIRS: [&str; 8] = [
    "sessions",
    "node_modules",
    "cache",
    "caches",
    // `log` singular too: harnesses spell it both ways, and a session log is
    // state either way (review F7).
    "log",
    "logs",
    "blobs",
    "run",
];

/// Suffixes that are state by construction.
const DENY_SUFFIXES: [&str; 9] = [
    ".db", ".db-wal", ".db-shm", ".sqlite", ".sqlite3", ".log", ".lock", ".pem", ".key",
];

/// SQLite sidecars and other database spellings the suffix list cannot express
/// as a single fixed suffix (review F7): `-wal`/`-shm`/`-journal` attach to
/// whichever extension the database chose (`sessions.sqlite-wal`,
/// `sessions.db-journal`), and `.db3` is a database too. Checked as
/// "a database stem plus a sidecar tail", so `notes-wal` (not a database) is not
/// swept up.
const DB_STEMS: [&str; 4] = [".db", ".sqlite", ".sqlite3", ".db3"];
const DB_TAILS: [&str; 4] = ["-wal", "-shm", "-journal", "-lock"];

/// Does `name` look like a database or one of its sidecars?
fn is_database_name(name: &str) -> bool {
    if DB_STEMS.iter().any(|stem| name.ends_with(stem)) {
        return true;
    }
    DB_STEMS.iter().any(|stem| {
        DB_TAILS
            .iter()
            .any(|tail| name.ends_with(&format!("{stem}{tail}")))
    })
}

/// Does `path` name something that must never sync?
///
/// The rules are the LOCAL column of `docs/harness-centralization.md` §1,
/// written as code so a user-declared path gets the same protection a preset
/// file does. The Codex hook-trust rule is content-level and lives in
/// [`deny_content`], because a `.toml` in the right place looks harmless until
/// you read it.
#[must_use]
pub fn deny_path(path: &std::path::Path) -> Option<DenyRule> {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lowered = name.to_lowercase();
        if DENY_NAMES.contains(&lowered.as_str()) {
            return Some(DenyRule::Credentials);
        }
        if lowered.starts_with("id_") && !lowered.ends_with(".pub") {
            return Some(DenyRule::KeyMaterial);
        }
        if is_database_name(&lowered) {
            return Some(DenyRule::Database);
        }
        for suffix in DENY_SUFFIXES {
            if lowered.ends_with(suffix) {
                return Some(match suffix {
                    ".db" | ".db-wal" | ".db-shm" | ".sqlite" | ".sqlite3" => DenyRule::Database,
                    ".pem" | ".key" => DenyRule::KeyMaterial,
                    _ => DenyRule::Noise,
                });
            }
        }
    }
    for component in path.components() {
        if let std::path::Component::Normal(part) = component {
            if let Some(text) = part.to_str() {
                if DENY_DIRS.contains(&text.to_lowercase().as_str()) {
                    return Some(DenyRule::StateDirectory);
                }
            }
        }
    }
    None
}

/// A content-level LOCAL rule: a TOML table path that names per-machine hook
/// trust hashes.
///
/// Codex's hook events carry `trusted_hash` entries that hash a *local* file.
/// They are different on every machine by construction, so a sync would rewrite
/// them on arrival, invalidate the peer's trust, and raise a trust prompt — the
/// exact "a sync that looks successful and breaks the peer" shape. Refused by
/// class, before the scan, because it is not a dirty file; it is an untransferable
/// one.
#[must_use]
pub fn deny_content(text: &str) -> Option<DenyRule> {
    // **Two spellings, because the measured one is not the tidy one.** The real
    // artifact is `[hooks.state."<abs path to hooks.json>:<event>:0:0"]` with
    // `trusted_hash = "sha256:…"` *inside* the table (docs/harness-centralization
    // §1; the live Orca-managed Codex config on this box). A rule keyed on the
    // table name ending in `.trusted_hash` therefore missed every real file —
    // and the peer's hashes would be rewritten, invalidating its hook trust and
    // raising a prompt: a sync that looks successful and breaks the peer. The
    // table name carries dots inside a quoted path, so the rule tracks the
    // `[hooks.state…]` header and then any `trusted_hash` assignment under it;
    // the older `[…trusted_hash]` spelling still matches.
    let mut in_hooks_state = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && line.ends_with(']') {
            let table = line.trim_matches(['[', ']']).trim();
            let parts: Vec<&str> = table.split('.').map(str::trim).collect();
            in_hooks_state = parts.first() == Some(&"hooks") && parts.get(1) == Some(&"state");
            if in_hooks_state && parts.last() == Some(&"trusted_hash") {
                return Some(DenyRule::HookTrustHash);
            }
            continue;
        }
        if in_hooks_state {
            let key = line.split('=').next().unwrap_or("").trim();
            if key == "trusted_hash" {
                return Some(DenyRule::HookTrustHash);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_registry_has_only_the_three_surveyed_harnesses() {
        let harnesses: Vec<&str> = presets().iter().map(|p| p.harness.id()).collect();
        assert_eq!(harnesses, ["opencode", "pi", "omp"]);
        // A harness with no preset has no dialect either — the refusal path for
        // "some other harness" is the absence of an entry, not a default.
        assert!(dialect_for("grok-cli").is_none());
        assert!(dialect_for("opencode").is_some());
    }

    #[test]
    fn the_worked_cases_files_are_syncable_and_their_neighbours_are_not() {
        // The two files the §3.8 case edits.
        for name in [
            "opencode.jsonc",
            "tui.jsonc",
            "models.json",
            "settings.json",
        ] {
            let (_, entry) = file(name).expect("surveyed");
            assert_eq!(entry.class, Class::Sync, "{name}");
        }
        // pi's credentials sit in the same directory as a syncable file.
        assert_eq!(file("auth.json").expect("surveyed").1.class, Class::Local);
        assert_eq!(file("opencode.db").expect("surveyed").1.class, Class::Local);
        assert_eq!(
            file("opencode.json")
                .expect("sibling is registered")
                .1
                .class,
            Class::Local
        );
    }

    #[test]
    fn a_sibling_extension_is_found_only_where_the_harness_merges_it() {
        let (_, jsonc) = file("opencode.jsonc").expect("surveyed");
        assert_eq!(
            sibling_of(jsonc, Path::new("/cfg/opencode/opencode.jsonc")),
            Some(PathBuf::from("/cfg/opencode/opencode.json"))
        );
        // pi merges no sibling, so a stray file beside models.json is not the
        // merge hazard opencode has.
        let (_, models) = file("models.json").expect("surveyed");
        assert_eq!(sibling_of(models, Path::new("/pi/models.json")), None);
    }

    #[test]
    fn the_local_deny_list_covers_the_surveys_own_examples() {
        assert_eq!(
            deny_path(Path::new("/home/x/.config/opencode/credentials.json")),
            Some(DenyRule::Credentials)
        );
        assert_eq!(
            deny_path(Path::new("/home/x/.local/share/opencode/opencode.db")),
            Some(DenyRule::Database)
        );
        assert_eq!(
            deny_path(Path::new("/home/x/.local/share/opencode/opencode.db-wal")),
            Some(DenyRule::Database)
        );
        assert_eq!(
            deny_path(Path::new("/pi/sessions/2026-09-13.jsonl")),
            Some(DenyRule::StateDirectory)
        );
        assert_eq!(
            deny_path(Path::new(
                "/cfg/opencode/node_modules/@ai-sdk/x/package.json"
            )),
            Some(DenyRule::StateDirectory)
        );
        assert_eq!(
            deny_path(Path::new("/var/log/arreo.log")),
            Some(DenyRule::Noise)
        );
        assert_eq!(
            deny_path(Path::new("/home/x/.ssh/id_ed25519")),
            Some(DenyRule::KeyMaterial)
        );
        // A portable intent file trips nothing: the list is a floor, not a
        // classifier that swallows everything it does not recognize.
        assert_eq!(deny_path(Path::new("/cfg/opencode/opencode.jsonc")), None);
        assert_eq!(deny_path(Path::new("/pi/models.json")), None);
    }

    #[test]
    fn a_codex_hook_trust_block_is_refused_by_class() {
        // **The measured spelling**, copied from the live Orca-managed Codex
        // config on this box: the table name carries the absolute path to
        // hooks.json, the event, and two indexes — and `trusted_hash` is a key
        // *inside* it. The tidy `[hooks.state.abc.trusted_hash]` form below never
        // occurs in the wild, and a rule keyed on it alone let every real file
        // through (review F4).
        let measured = "[hooks.state.\"/home/u/.config/orca/hooks.json:session_start:0:0\"]\n\
                        enabled = true\n\
                        trusted_hash = \"sha256:d88f619c310bc84a3064045df2970d4148569998de77e084807643b60ac9cd0f\"\n";
        assert_eq!(deny_content(measured), Some(DenyRule::HookTrustHash));
        // The older spelling still matches.
        let tidy = "[hooks.state.abc123.trusted_hash]\nhash = \"deadbeef\"\n";
        assert_eq!(deny_content(tidy), Some(DenyRule::HookTrustHash));
        // A `hooks.state` table without a hash is ordinary config.
        let plain = "[hooks.state.abc123]\ncommand = \"arreo\"\n";
        assert_eq!(deny_content(plain), None);
        // And a `trusted_hash` outside `hooks.state` is not hook trust: the rule
        // is about where the key sits, not about the word.
        let elsewhere = "[other]\ntrusted_hash = \"deadbeef\"\n";
        assert_eq!(deny_content(elsewhere), None);
    }

    /// The sidecar and key-material spellings the suffix list cannot express
    /// (review F7). Each of these passed the class check before.
    #[test]
    fn database_sidecars_and_host_key_material_are_refused() {
        for path in [
            "/cfg/sessions.sqlite-wal",
            "/cfg/sessions.sqlite-shm",
            "/cfg/sessions.db-journal",
            "/cfg/sessions.db3",
            "/cfg/arreo/log/session.json",
            "/cfg/x/authorized_keys",
            "/cfg/x/known_hosts",
            "/cfg/x/.env",
        ] {
            assert!(
                deny_path(Path::new(path)).is_some(),
                "{path} must be refused by class"
            );
        }
        // A file that merely ends in `-wal` is not a database sidecar.
        assert_eq!(deny_path(Path::new("/cfg/notes-wal")), None);
    }

    #[test]
    fn each_dialect_spells_the_reference_its_harness_measured() {
        assert_eq!(
            Dialect::OpencodeEnvBrace.native("VBK_PROD_KEY"),
            "{env:VBK_PROD_KEY}"
        );
        assert_eq!(Dialect::PiDollar.native("VBK_PROD_KEY"), "${VBK_PROD_KEY}");
        // omp takes the bare name and nothing else (T-0080, measured through a
        // proxy that logged what was actually sent).
        assert_eq!(Dialect::OmpBareName.native("VBK_PROD_KEY"), "VBK_PROD_KEY");
    }
}
