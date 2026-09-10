//! Daemon lifecycle (T-0012): service unit generators + shutdown drain.
//!
//! One sentence: install the daemon as a first-class OS service everywhere,
//! stop it without losing committed output, and say honestly what crash
//! recovery can and cannot do before T-0018 persistence lands.
//!
//! - `unit_file(kind)`: render the systemd user unit / launchd plist /
//!   Windows Service wrapper script for the current binary. Unit files are
//!   plain text we generate (no service-manager crates — fewer deps, fully
//!   auditable, user edits welcome).
//! - `shutdown_deadline()`: SIGTERM → stop accepting, flush panes, exit 0
//!   within 5 s. Crash (`kill -9`) recovery of live sessions needs T-0018;
//!   until then restart comes back with an empty registry and says so.

use std::path::{Path, PathBuf};

/// Which service manager to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// systemd user unit (`systemctl --user`). Linux.
    SystemdUser,
    /// launchd agent plist (`~/Library/LaunchAgents`). macOS.
    Launchd,
    /// Windows Service wrapper (via `sc.exe` script). Windows.
    WindowsService,
}

impl ServiceKind {
    /// The native manager for this OS.
    #[must_use]
    pub fn native() -> Self {
        #[cfg(target_os = "windows")]
        return Self::WindowsService;
        #[cfg(target_os = "macos")]
        return Self::Launchd;
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Self::SystemdUser;
    }
}

/// Render the unit file for `binary` + `socket`.
#[must_use]
pub fn unit_file(kind: ServiceKind, binary: &Path, socket: &Path) -> String {
    match kind {
        ServiceKind::SystemdUser => format!(
            "[Unit]\n\
             Description=Arreo daemon (PTY sessions + socket API)\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={} --socket {}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             # Hardening: no new privileges, private /tmp (socket lives in\n\
             # $XDG_RUNTIME_DIR, unaffected).\n\
             NoNewPrivileges=true\n\
             PrivateTmp=true\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            binary.display(),
            socket.display()
        ),
        ServiceKind::Launchd => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key><string>dev.arreo.daemon</string>\n\
             \t<key>ProgramArguments</key>\n\
             \t<array>\n\
             \t\t<string>{}</string>\n\
             \t\t<string>--socket</string>\n\
             \t\t<string>{}</string>\n\
             \t</array>\n\
             \t<key>RunAtLoad</key><true/>\n\
             \t<key>KeepAlive</key><true/>\n\
             </dict>\n\
             </plist>\n",
            binary.display(),
            socket.display()
        ),
        ServiceKind::WindowsService => format!(
            "@echo off\r\n\
             REM Arreo daemon as a Windows Service (run as Administrator).\r\n\
             REM Requires a service wrapper (NSSM or WinSW); the daemon itself\r\n\
             REM handles service-stop as SIGTERM-equivalent via Ctrl events.\r\n\
             sc.exe create ArreoDaemon binPath= \"\\\"{}\\\" --socket \\\"{}\\\"\" start= auto\r\n\
             sc.exe start ArreoDaemon\r\n",
            binary.display(),
            socket.display()
        ),
    }
}

/// Where `service install` writes the unit for `kind`.
#[must_use]
pub fn unit_path(kind: ServiceKind) -> Option<PathBuf> {
    match kind {
        ServiceKind::SystemdUser => std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/systemd/user/arreo.service")),
        ServiceKind::Launchd => std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join("Library/LaunchAgents/dev.arreo.daemon.plist")),
        ServiceKind::WindowsService => None,
    }
}

/// Graceful-shutdown drain deadline: SIGTERM → exit(0) within this long.
pub const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Snapshot of what shutdown must preserve (for the drain routine + tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    /// Panes whose committed ring-buffer output was flushed.
    pub flushed_panes: usize,
    /// Panes still alive at shutdown (children keep running — the daemon
    /// exits, it does not kill agents; T-0018 re-attaches on restart).
    pub orphaned_panes: usize,
}
