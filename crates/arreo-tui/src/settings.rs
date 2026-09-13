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
    /// The *file's* answer to "quitting this TUI also drain-stops the local
    /// daemon" (T-0073). `false` is the default and the common case; the
    /// run's `--shutdown-on-exit`/`--no-shutdown-on-exit` overrule it, which
    /// is why the flag's answer is composed with this in
    /// [`shutdown_on_exit`] rather than folded in here.
    pub exit_kills_daemon: bool,
}

/// The run's answer to "quitting stops the local daemon" (T-0073): the flag
/// wins, the config file is the default, and neither means off.
///
/// A separate function because the two sources have different strengths: the
/// flag is a decision about *this run* and the file is a standing preference,
/// so `--no-shutdown-on-exit` must be able to overrule a file that says true
/// (a script quitting a TUI on a machine configured for the opt-in must be
/// able to say no).
#[must_use]
pub fn shutdown_on_exit(flag: Option<bool>, file: bool) -> bool {
    flag.unwrap_or(file)
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
        exit_kills_daemon: false,
    };
    let Some(path) = config_path(explicit) else {
        return (settings, None);
    };
    match TuiSettings::load(&path) {
        // The file can only ever ask for still.
        Ok(file) => {
            settings.reduce_motion |= file.reduce_motion == Some(true);
            settings.exit_kills_daemon = file.exit_kills_daemon == Some(true);
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

    /// The opt-in (T-0073): off unless the file asks, and the flag is the
    /// run's word over the file's standing preference.
    #[test]
    fn the_daemon_stop_is_off_unless_asked_and_the_flag_beats_the_file() {
        // Nothing asked: off. This is the default the whole ticket turns on.
        assert!(!resolve(None, Depth::Truecolor).0.exit_kills_daemon);
        assert!(!shutdown_on_exit(None, false));

        let on = temp_config("exit-on", "[tui]\nexit_kills_daemon = true\n");
        assert!(
            resolve(Some(&on), Depth::Truecolor).0.exit_kills_daemon,
            "the documented key must work"
        );
        assert!(shutdown_on_exit(None, true), "the file is the default");
        let _ = std::fs::remove_file(&on);

        // The flag wins in both directions: a file that says true is overruled
        // by `--no-shutdown-on-exit`, and a file that says false by
        // `--shutdown-on-exit`.
        assert!(
            !shutdown_on_exit(Some(false), true),
            "--no-shutdown-on-exit must overrule a config that says true"
        );
        assert!(shutdown_on_exit(Some(true), false));

        let off = temp_config("exit-off", "[tui]\nexit_kills_daemon = false\n");
        assert!(!resolve(Some(&off), Depth::Truecolor).0.exit_kills_daemon);
        let _ = std::fs::remove_file(&off);

        // A broken file is not a request for the opt-in, and it is reported.
        let broken = temp_config("exit-broken", "[tui\nexit_kills_daemon = ");
        let (settings, problem) = resolve(Some(&broken), Depth::Truecolor);
        assert!(!settings.exit_kills_daemon);
        assert!(
            problem.is_some(),
            "a broken file must reach the status line"
        );
        let _ = std::fs::remove_file(&broken);

        // A key of the wrong type is the same kind of fact (T-0073 reuses the
        // `reduce_motion` contract), not a silent `false`.
        let wrong = temp_config("exit-wrong", "[tui]\nexit_kills_daemon = \"yes\"\n");
        let (settings, problem) = resolve(Some(&wrong), Depth::Truecolor);
        assert!(!settings.exit_kills_daemon);
        assert!(
            problem
                .as_ref()
                .is_some_and(|p| p.contains("exit_kills_daemon")),
            "the report must name the key: {problem:?}"
        );
        let _ = std::fs::remove_file(&wrong);
    }
}
