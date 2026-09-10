//! Theme discovery and loading (T-0016).
//!
//! One sentence: built-ins are embedded in the binary, every later directory
//! on the search path overrides them by name, and a theme file is validated
//! before it can affect a single pixel.
//!
//! Hierarchy (§3.12): built-in → `$XDG_CONFIG_HOME/arreo/themes` (usually
//! `~/.config/arreo/themes`) → `<project root>/.arreo/themes` → `./.arreo/themes`.

use crate::theme::color::{Color, Depth};
use crate::theme::schema::{self, RawTheme, SchemaError, Variant};
use crate::theme::{Theme, BASE_THEME};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Built-in themes, embedded so a static binary works with no data dir.
pub const BUILTINS: &[(&str, &str)] = &[
    ("arreo", include_str!("../../themes/arreo.json")),
    ("tokyonight", include_str!("../../themes/tokyonight.json")),
    ("catppuccin", include_str!("../../themes/catppuccin.json")),
    ("gruvbox", include_str!("../../themes/gruvbox.json")),
    ("system", include_str!("../../themes/system.json")),
];

/// Everything that can go wrong between a theme name and a renderable theme.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("theme {name:?} not found (built-ins: {available})")]
    NotFound { name: String, available: String },
    #[error("theme {theme:?} in {path} is invalid: {source}")]
    Invalid {
        theme: String,
        path: PathBuf,
        source: Box<SchemaError>,
    },
    #[error("cannot read {path}: {detail}")]
    Io { path: PathBuf, detail: String },
}

/// The themes available to this process, in load order.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    raw: RawTheme,
    /// `None` for built-ins; the file path for anything on disk.
    path: Option<PathBuf>,
}

impl Catalog {
    /// The embedded themes only.
    #[must_use]
    pub fn builtin() -> Self {
        let mut catalog = Self::default();
        for (name, json) in BUILTINS {
            let raw: RawTheme = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("built-in theme {name} is not valid JSON: {e}"));
            catalog
                .entries
                .insert((*name).to_string(), Entry { raw, path: None });
        }
        catalog
    }

    /// Built-ins plus every `*.json` in the given directories, later wins.
    /// A missing directory is not an error (most users have neither).
    pub fn discover(dirs: &[PathBuf]) -> Result<Self, LoadError> {
        let mut catalog = Self::builtin();
        for dir in dirs {
            let Ok(read) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            // Deterministic order: two files cannot fight over a name by
            // filesystem enumeration order.
            files.sort();
            for path in files {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default();
                let text = std::fs::read_to_string(&path).map_err(|e| LoadError::Io {
                    path: path.clone(),
                    detail: e.to_string(),
                })?;
                let raw: RawTheme =
                    serde_json::from_str(&text).map_err(|e| LoadError::Invalid {
                        theme: name.clone(),
                        path: path.clone(),
                        source: Box::new(SchemaError::Json {
                            theme: name.clone(),
                            detail: e.to_string(),
                        }),
                    })?;
                catalog.entries.insert(
                    name,
                    Entry {
                        raw,
                        path: Some(path),
                    },
                );
            }
        }
        Ok(catalog)
    }

    /// Search path: user config, the project root, then the working
    /// directory. `ARREO_THEME_DIR` prepends (used by tests and by people who
    /// keep themes somewhere else entirely).
    #[must_use]
    pub fn default_dirs() -> Vec<PathBuf> {
        default_dirs()
    }

    #[must_use]
    pub fn builtin_names() -> Vec<String> {
        BUILTINS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect()
    }

    /// All theme names, built-ins first (that is also the picker's order).
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTINS.iter().map(|(n, _)| (*n).to_string()).collect();
        for name in self.entries.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Where a theme came from (`None` = embedded), for `/theme` display.
    #[must_use]
    pub fn source(&self, name: &str) -> Option<&Path> {
        self.entries
            .get(name)
            .and_then(|entry| entry.path.as_deref())
    }

    /// Resolve a theme at the process's current terminal depth.
    pub fn theme(&self, name: &str, variant: Variant) -> Result<Theme, LoadError> {
        self.theme_with_depth(name, variant, Depth::detect())
    }

    /// Resolve a theme for an explicit variant and depth.
    pub fn theme_with_depth(
        &self,
        name: &str,
        variant: Variant,
        depth: Depth,
    ) -> Result<Theme, LoadError> {
        let entry = self.entries.get(name).ok_or_else(|| LoadError::NotFound {
            name: name.to_string(),
            available: Catalog::builtin_names().join(", "),
        })?;
        let path = entry
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<builtin>"));
        let mut colors = validate(name, &path, &entry.raw, variant)?;
        // Partial themes inherit the rest of the base look, so a theme file
        // with three tokens is a valid theme, not a broken window.
        if name != BASE_THEME {
            if let Some(base) = self.entries.get(BASE_THEME) {
                let base_colors = validate(
                    BASE_THEME,
                    &base
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("<builtin>")),
                    &base.raw,
                    variant,
                )?;
                for (token, color) in base_colors {
                    colors.entry(token).or_insert(color);
                }
            }
        }
        Ok(Theme::new(name, variant, depth, colors))
    }
}

fn validate(
    name: &str,
    path: &Path,
    raw: &RawTheme,
    variant: Variant,
) -> Result<BTreeMap<String, Color>, LoadError> {
    schema::resolve(name, raw, variant).map_err(|source| LoadError::Invalid {
        theme: name.to_string(),
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// `ARREO_THEME_DIR` (if set), `$XDG_CONFIG_HOME/arreo/themes` (or
/// `~/.config/arreo/themes`), `<project root>/.arreo/themes`, `./.arreo/themes`.
#[must_use]
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(custom) = std::env::var_os("ARREO_THEME_DIR") {
        dirs.push(PathBuf::from(custom));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(config).join("arreo").join("themes"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            PathBuf::from(home)
                .join(".config")
                .join("arreo")
                .join("themes"),
        );
    }
    if let Some(root) = project_root(&std::env::current_dir().unwrap_or_default()) {
        dirs.push(root.join(".arreo").join("themes"));
    }
    dirs.push(PathBuf::from(".arreo").join("themes"));
    dirs.dedup();
    dirs
}

/// Nearest ancestor containing `.git`, i.e. the project, when there is one.
#[must_use]
pub fn project_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_loads_in_both_variants() {
        let catalog = Catalog::builtin();
        for name in Catalog::builtin_names() {
            for variant in [Variant::Dark, Variant::Light] {
                let theme = catalog
                    .theme_with_depth(&name, variant, Depth::Truecolor)
                    .unwrap_or_else(|e| panic!("{name} ({}) failed: {e}", variant.as_str()));
                assert!(theme.colors().len() > 10, "{name} resolved too few tokens");
            }
        }
    }

    #[test]
    fn the_roadmap_minimum_builtins_ship() {
        let names = Catalog::builtin_names();
        for required in ["arreo", "tokyonight", "catppuccin", "gruvbox", "system"] {
            assert!(names.contains(&required.to_string()), "{required} missing");
        }
    }

    #[test]
    fn system_theme_is_terminal_native() {
        let catalog = Catalog::builtin();
        let theme = catalog
            .theme_with_depth("system", Variant::Dark, Depth::Truecolor)
            .expect("system loads");
        // No 24-bit color: it must blend with whatever palette the terminal
        // has, so backgrounds are unset and colors are ANSI indices.
        assert_eq!(theme.color("background"), Color::None);
        assert_eq!(theme.color("backgroundPanel"), Color::None);
        for token in [
            "primary", "text", "working", "blocked", "done", "idle", "question",
        ] {
            assert!(
                matches!(theme.color(token), Color::Ansi(_)),
                "{token} is not an ANSI index"
            );
        }
    }

    #[test]
    fn partial_themes_inherit_the_base_look() {
        let dir = tempdir("partial");
        std::fs::write(
            dir.join("mine.json"),
            r##"{ "theme": { "primary": "#ff0000" } }"##,
        )
        .expect("write");
        let catalog = Catalog::discover(std::slice::from_ref(&dir)).expect("discover");
        let theme = catalog
            .theme_with_depth("mine", Variant::Dark, Depth::Truecolor)
            .expect("loads");
        assert_eq!(theme.color("primary"), Color::Rgb(0xff, 0x00, 0x00));
        assert_eq!(
            theme.color("done"),
            Color::Rgb(0x9e, 0xce, 0x6a),
            "base inherited"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn later_directories_win() {
        let user = tempdir("user");
        let project = tempdir("project");
        std::fs::write(
            user.join("brand.json"),
            r##"{ "theme": { "primary": "#111111" } }"##,
        )
        .expect("write");
        std::fs::write(
            project.join("brand.json"),
            r##"{ "theme": { "primary": "#222222" } }"##,
        )
        .expect("write");
        let catalog = Catalog::discover(&[user.clone(), project.clone()]).expect("discover");
        assert_eq!(
            catalog.source("brand"),
            Some(project.join("brand.json").as_path())
        );
        let theme = catalog
            .theme_with_depth("brand", Variant::Dark, Depth::Truecolor)
            .expect("loads");
        assert_eq!(theme.color("primary"), Color::Rgb(0x22, 0x22, 0x22));
        std::fs::remove_dir_all(&user).ok();
        std::fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn a_broken_file_names_itself_and_the_reason() {
        let dir = tempdir("broken");
        std::fs::write(
            dir.join("bad.json"),
            r##"{ "theme": { "primry": "#fff" } }"##,
        )
        .expect("write");
        let catalog = Catalog::discover(std::slice::from_ref(&dir)).expect("discover is lazy");
        match catalog.theme_with_depth("bad", Variant::Dark, Depth::Truecolor) {
            Err(LoadError::Invalid { source, path, .. }) => {
                assert!(path.ends_with("bad.json"));
                assert!(source.to_string().contains("primry"), "{source}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_theme_says_what_exists() {
        let catalog = Catalog::builtin();
        match catalog.theme_with_depth("nope", Variant::Dark, Depth::Truecolor) {
            Err(LoadError::NotFound { name, available }) => {
                assert_eq!(name, "nope");
                assert!(available.contains("tokyonight"), "{available}");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn project_root_walks_up_to_the_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("repo root")
            .to_path_buf();
        let here = root.join("crates").join("arreo-core").join("src");
        assert_eq!(project_root(&here), Some(root));
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-theme-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tempdir");
        dir
    }
}
