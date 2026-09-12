//! TUI settings (T-0076): what this run should do differently.
//!
//! One sentence: the *file* is parsed once, in `arreo_core::relay::config`
//! (which owns the shape of the arreo config file — see its module docs), and
//! this module is the TUI's own policy on top of it: which config file this
//! process reads, and what the environment already decided.
//!
//! Two rules, both here because both are about the *terminal* rather than about
//! the file:
//!
//! - **`NO_COLOR` implies still.** A terminal with no color is a terminal where
//!   a blink would be the only signal left for `question`, and the answer for
//!   "the state is carried by motion alone" is the same as for color alone:
//!   don't. `[tui] reduce_motion = false` cannot overrule it — the file may ask
//!   for *more* stillness, never less.
//! - **A config the user named is either used or reported.** A missing file, or
//!   a file with no `[tui]` section, is the normal case and says nothing; a file
//!   that does not parse gets one line in the status bar, because the user
//!   pointed at it and would otherwise never learn it was ignored.

use arreo_core::relay::config::TuiSettings;
use arreo_core::theme::Depth;
use std::path::{Path, PathBuf};

/// What the UI should do differently, resolved for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// No blinking, no pulsing: the `question` group keeps its dot shape and
    /// its label, and stays still.
    pub reduce_motion: bool,
}

/// The config file this process should read: `--config`, else `$ARREO_CONFIG`
/// — the same precedence the daemon and `--machine` use, so one file configures
/// the machine however it is reached.
#[must_use]
pub fn config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("ARREO_CONFIG").map(PathBuf::from))
}

/// Resolve the settings for this run: the file's `[tui]` section, the
/// terminal's own answer, and — when there is something to say — a line for the
/// status bar.
#[must_use]
pub fn resolve(explicit: Option<&Path>, depth: Depth) -> (Settings, Option<String>) {
    // Motion is the last signal left on a NO_COLOR terminal; do not spend it.
    let mut settings = Settings {
        reduce_motion: depth == Depth::NoColor,
    };
    let Some(path) = config_path(explicit) else {
        return (settings, None);
    };
    match TuiSettings::load(&path) {
        // The file can only ever ask for still.
        Ok(file) => {
            settings.reduce_motion |= file.reduce_motion == Some(true);
            (settings, None)
        }
        Err(error) => (settings, Some(format!("tui settings: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(tag: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "arreo-tui-settings-{tag}-{}-{:?}.toml",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, body).expect("write temp config");
        path
    }

    #[test]
    fn no_config_means_the_terminal_decides() {
        let (settings, problem) = resolve(None, Depth::Truecolor);
        assert!(!settings.reduce_motion);
        assert_eq!(problem, None, "nothing was asked for and nothing to report");
    }

    #[test]
    fn no_color_implies_still_and_the_file_cannot_override_it() {
        // The key case: a file that explicitly asks for motion, on a terminal
        // that cannot show color. Still wins.
        let path = temp_config("no-color", "[tui]\nreduce_motion = false\n");
        let (settings, problem) = resolve(Some(&path), Depth::NoColor);
        assert!(settings.reduce_motion, "NO_COLOR implies still");
        assert_eq!(problem, None);
        let _ = std::fs::remove_file(&path);

        let (settings, _) = resolve(None, Depth::NoColor);
        assert!(settings.reduce_motion, "and without a file at all");
    }

    #[test]
    fn the_documented_key_turns_motion_off_and_a_broken_file_is_reported() {
        let path = temp_config("still", "[tui]\nreduce_motion = true\n");
        let (settings, problem) = resolve(Some(&path), Depth::Truecolor);
        assert!(settings.reduce_motion, "the documented key must work");
        assert_eq!(problem, None);
        let _ = std::fs::remove_file(&path);

        let broken = temp_config("broken", "[tui\nreduce_motion = ");
        let (settings, problem) = resolve(Some(&broken), Depth::Truecolor);
        assert!(
            !settings.reduce_motion,
            "a typo is not a request for motion"
        );
        assert!(
            problem
                .as_ref()
                .is_some_and(|p| p.starts_with("tui settings:")),
            "a broken config must reach the status line: {problem:?}"
        );
        let _ = std::fs::remove_file(&broken);

        // A file without the section (the daemon's relay config, typically) is
        // not a problem to report.
        let relay_only = temp_config("relay-only", "[relay]\nenabled = false\n");
        let (settings, problem) = resolve(Some(&relay_only), Depth::Truecolor);
        assert!(!settings.reduce_motion);
        assert_eq!(problem, None);
        let _ = std::fs::remove_file(&relay_only);
    }

    #[test]
    fn the_explicit_path_beats_the_environment() {
        let explicit = PathBuf::from("/tmp/explicit.toml");
        assert_eq!(
            config_path(Some(&explicit)),
            Some(explicit),
            "--config is the explicit choice"
        );
    }
}
