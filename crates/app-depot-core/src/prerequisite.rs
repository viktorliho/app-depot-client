//! Platform boundary for explicitly authorized Standard prerequisites.
use crate::installation::Prerequisite;
use std::path::{Path, PathBuf};

/// An independent, verified copy of the source Artifact. Tree paths are
/// relative to `root`; a File Artifact has no declared relative name.
#[derive(Debug, Clone)]
pub struct PreparedPrerequisiteSource {
    pub root: PathBuf,
    pub file: PathBuf,
    pub directory: PathBuf,
}

/// The coordinator has verified the complete source layout and recorded
/// authorization before calling the executor. Never use the CAS directly.
/// Execute only `source.file`, with `source.directory` as working directory,
/// literal arguments, no shell, automatic elevation or external tools.
/// A non-ambiguous outcome must mean the executor no longer uses the layout.
pub struct PrerequisiteRequest<'a> {
    pub generation_id: &'a str,
    pub attempt_id: &'a str,
    pub payload: &'a Path,
    pub source: &'a PreparedPrerequisiteSource,
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
    /// this exact generation. The source context is a freshly verified copy;
    /// attempt_id identifies the earlier execution. Default is conservative.
    fn verify_completion(&mut self, _request: &PrerequisiteRequest<'_>) -> bool {
        false
    }
}
