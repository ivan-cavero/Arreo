//! Enforcement matrix stubs (non-Linux): honest, never fake numbers.
//!
//! Windows = Job Objects (`JOBOBJECT_EXTENDED_LIMIT_INFORMATION` with
//! `JOB_OBJECT_LIMIT_PROCESS_MEMORY` + `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`),
//! macOS = advisory (rlimit + monitor + kill, per ROADMAP §3.11). Both land
//! with real implementations when their platform work starts — until then
//! every method says exactly where it belongs.

use super::{Breach, Budget, EnforceError};
use std::path::Path;

pub struct Guard;

impl Guard {
    pub fn create(_name: &str, _budget: Budget) -> Result<Self, EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    pub fn attach(&self, _pid: u32) -> Result<(), EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    pub fn breached(&self) -> Result<Option<Breach>, EnforceError> {
        Err(EnforceError::Unimplemented("T-0019 (resource enforcement)"))
    }

    pub fn path(&self) -> &Path {
        Path::new("")
    }
}

pub struct OtherProbe;
