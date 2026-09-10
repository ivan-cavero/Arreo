//! Non-Linux probe: honest stub (real impls land with T-0010/T-0019).

use super::{Probe, ProbeError, TreeSample};

pub struct OtherProbe;

impl Probe for OtherProbe {
    fn sample_tree(&self, _root: u32) -> Result<TreeSample, ProbeError> {
        Err(ProbeError::Unimplemented("T-0019 (resource enforcement)"))
    }

    fn clock_ticks_per_sec(&self) -> u64 {
        100
    }
}
