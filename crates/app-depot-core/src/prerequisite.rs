//! Platform boundary for explicitly authorized Standard prerequisites.
use crate::installation::Prerequisite;
use std::path::Path;

/// The coordinator has verified the source and recorded authorization before
/// calling the executor. The executor must independently stage and verify the
/// executable, never execute or expose the CAS object to an installer, and use
/// literal arguments without a shell, automatic elevation or external tools.
pub struct PrerequisiteRequest<'a> {
    pub generation_id: &'a str,
    pub attempt_id: &'a str,
    pub payload: &'a Path,
    pub verified_source: &'a Path,
    pub prerequisite: &'a Prerequisite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrerequisiteOutcome {
    Exited(i32),
    Failed(String),
    Cancelled,
    Ambiguous(String),
}

pub trait PrerequisiteExecutor {
    fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome;

    /// Confirm that an earlier successful attempt's effects remain valid in
    /// this exact generation. Default is conservative: require renewed consent.
    fn verify_completion(&mut self, _request: &PrerequisiteRequest<'_>) -> bool {
        false
    }
}
