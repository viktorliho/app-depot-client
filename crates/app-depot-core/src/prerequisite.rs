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
}

pub struct NativeExecutor;

impl PrerequisiteExecutor for NativeExecutor {
    fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
        #[cfg(windows)]
        {
            execute_windows(request)
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            PrerequisiteOutcome::Failed(
                "Native prerequisite execution is unavailable on this platform".into(),
            )
        }
    }
}

#[cfg(windows)]
fn execute_windows(request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
    use crate::{windows_fs, windows_process, windows_supervisor};
    let source = request.source;
    let launch = || -> std::io::Result<_> {
        let invalid = || std::io::Error::other("Invalid prepared prerequisite path");
        let owned = source.root.parent().ok_or_else(invalid)?;
        let relative = |path: &Path| {
            path.strip_prefix(owned)
                .ok()
                .and_then(Path::to_str)
                .map(|s| s.replace('\\', "/"))
                .ok_or_else(invalid)
        };
        let target = relative(&source.file)?;
        let work = relative(&source.directory)?;
        let _binding = windows_fs::bind_image(owned, request.attempt_id, &target, &work, true)?;
        let child = match windows_process::create_suspended(
            &source.file,
            &source.directory,
            &request.prerequisite.arguments,
        ) {
            Ok(child) => child,
            Err(error)
                if error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_ELEVATION_REQUIRED as i32) =>
            {
                return Ok(Err(windows_process::authorized_elevation(
                    &source.file,
                    &source.directory,
                    &request.prerequisite.arguments,
                )));
            }
            Err(error) => return Err(error),
        };
        let job = windows_supervisor::new_job(uuid::Uuid::new_v4(), &child)?;
        Ok(Ok((child.resume()?, job)))
    };
    let (mut child, job) = match launch() {
        Ok(Ok(pair)) => pair,
        Ok(Err(outcome)) => return outcome,
        Err(error) => return PrerequisiteOutcome::Failed(error.to_string()),
    };
    let mut completion = || -> std::io::Result<i32> {
        let status = child.wait()?;
        while windows_supervisor::active(&job)? {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        status
            .code()
            .ok_or_else(|| std::io::Error::other("Missing Windows process exit code"))
    };
    match completion() {
        Ok(code) => PrerequisiteOutcome::Exited(code),
        Err(error) => PrerequisiteOutcome::Ambiguous(error.to_string()),
    }
}
