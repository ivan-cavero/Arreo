//! Enforcement matrix stubs (non-Linux): honest, never fake numbers.
//!
//! Windows = Job Objects (`JOBOBJECT_EXTENDED_LIMIT_INFORMATION` with
//! `JOB_OBJECT_LIMIT_PROCESS_MEMORY` + `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`),
//! macOS = advisory (rlimit + monitor + kill, per ROADMAP §3.11). Both land
//! with real implementations when their platform work starts — until then
//! every method says exactly where it belongs.

use super::{Breach, Budget, EnforceError, Pressure};
use std::path::Path;

pub struct Guard;

impl Guard {
    pub fn create(_name: &str, _budget: Budget) -> Result<Self, EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    /// No group exists on this OS to adopt: a handoff that carried a guard path
    /// has nothing to re-open, and says so rather than serving the pane bare.
    pub fn reopen(_path: std::path::PathBuf) -> Result<Self, EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    /// The validate-only half of [`Guard::reopen`], used by the incoming daemon
    /// before the cut: no group exists on this OS, so no path is adoptable, and
    /// the pre-commit check must refuse with the same honesty `reopen` would.
    pub fn validate(_path: &std::path::Path) -> Result<(), EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    pub fn attach(&self, _pid: u32) -> Result<(), EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    pub fn breached(&self) -> Result<Option<Breach>, EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    /// No group exists on this OS, so there is no pressure to read: the honest
    /// empty reading (a gap, never an error — T-0041).
    #[must_use]
    pub fn pressure(&self) -> Pressure {
        Pressure::default()
    }

    pub fn path(&self) -> &Path {
        Path::new("")
    }
}

pub struct OtherProbe;
