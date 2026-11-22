//! Native Windows process creation and identity for the managed lifecycle.
use std::{
    ffi::OsStr,
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    ptr::null,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess,
        GetProcessTimes, INFINITE, OpenProcess, PROCESS_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread, STARTUPINFOW,
        TerminateProcess, WaitForSingleObject,
    },
};

pub struct WindowsChild {
    handle: OwnedHandle,
    id: u32,
}

impl WindowsChild {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_for(INFINITE)?
            .ok_or_else(|| io::Error::other("unexpected infinite wait timeout"))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_for(0)
    }

    fn wait_for(&self, timeout: u32) -> io::Result<Option<ExitStatus>> {
        let handle = self.handle.as_raw_handle();
        // SAFETY: the owned process handle stays live throughout both calls.
        unsafe {
            match WaitForSingleObject(handle, timeout) {
                WAIT_OBJECT_0 => {
                    let mut exit = 0;
                    if GetExitCodeProcess(handle, &mut exit) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(Some(ExitStatus::from_raw(exit)))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
}

pub(crate) fn create_suspended(
    image: &Path,
    working_directory: &Path,
    arguments: &[String],
) -> io::Result<Suspended> {
    let mut command = Vec::new();
    append_arg(image.as_os_str(), &mut command);
    for argument in arguments {
        command.push(u16::from(b' '));
        append_arg(OsStr::new(argument), &mut command);
    }
    command.push(0);
    if command.len() > 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows argument limit exceeded",
        ));
    }
    let mut variables: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !crate::launch::sensitive_env_name(key))
        .collect();
    variables.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut environment = Vec::new();
    for (key, value) in variables {
        environment.extend(key.encode_wide());
        environment.push(u16::from(b'='));
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    if environment.is_empty() {
        environment.push(0);
    }
    environment.push(0);
    let image: Vec<_> = image.as_os_str().encode_wide().chain([0]).collect();
    let work: Vec<_> = working_directory
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: all strings and the environment are terminated, command is writable, and
    // the output is initialized by CreateProcessW. The caller supplies pinned absolute paths.
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            work.as_ptr(),
            &startup,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful CreateProcessW returned two distinct owned handles.
    Ok(unsafe {
        Suspended {
            child: Some(WindowsChild {
                handle: OwnedHandle::from_raw_handle(process.hProcess),
                id: process.dwProcessId,
            }),
            thread: OwnedHandle::from_raw_handle(process.hThread),
        }
    })
}

// The Windows CRT convention quotes each argument and doubles backslashes before quotes.
fn append_arg(arg: &OsStr, output: &mut Vec<u16>) {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    output.push(QUOTE);
    let mut slashes = 0;
    for item in arg.encode_wide() {
        if item == SLASH {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            SLASH,
            if item == QUOTE {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        output.push(item);
    }
    output.extend(std::iter::repeat_n(SLASH, slashes * 2));
    output.push(QUOTE);
}

pub(crate) struct Suspended {
    child: Option<WindowsChild>,
    thread: OwnedHandle,
}

impl Suspended {
    pub(crate) fn identity(&self) -> io::Result<(u32, u64)> {
        let child = self.child.as_ref().unwrap();
        Ok((child.id(), creation_time(child.handle.as_raw_handle())?))
    }

    pub(crate) fn resume(mut self) -> io::Result<WindowsChild> {
        // SAFETY: the primary thread handle is owned and live.
        if unsafe { ResumeThread(self.thread.as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(self.child.take().unwrap())
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        if let Some(child) = &self.child {
            // SAFETY: this is the handle of the suspended process we just created.
            unsafe { TerminateProcess(child.handle.as_raw_handle(), 1) };
        }
    }
}

fn creation_time(process: HANDLE) -> io::Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: process is held by the caller and each output points to a FILETIME.
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

pub(crate) fn is_running(id: u32, started: u64) -> io::Result<bool> {
    // SAFETY: OpenProcess returns a fresh handle with query and synchronization rights.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            id,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    // SAFETY: successful OpenProcess returned ownership of raw.
    let process = WindowsChild {
        handle: unsafe { OwnedHandle::from_raw_handle(raw) },
        id,
    };
    Ok(creation_time(process.handle.as_raw_handle())? == started && process.wait_for(0)?.is_none())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Idle,
    Running,
    Unknown,
}

use crate::{
    ServerProfileId,
    library::{Library, LibraryError},
    windows_fs,
};
use rusqlite::params;

impl Library {
    pub fn windows_process_state(&self, installation: &str) -> Result<ProcessState, LibraryError> {
        let (pid, time): (Option<i64>, Option<String>) = self.conn.query_row(
            "SELECT process_id,process_start FROM installations WHERE id=?1",
            [installation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(self::process_state(pid, time.as_deref()))
    }

    pub fn windows_active_processes(&self) -> Result<Vec<(String, ProcessState)>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT id,process_id,process_start FROM installations WHERE process_id IS NOT NULL OR process_start IS NOT NULL ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            let time: Option<String> = row.get(2)?;
            Ok((row.get(0)?, process_state(row.get(1)?, time.as_deref())))
        })?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, state)| *state != ProcessState::Idle)
            .collect())
    }

    pub fn play_declared(
        &self,
        profile: &ServerProfileId,
        release: &str,
        association: &str,
        entry: &str,
        revision: &str,
    ) -> Result<WindowsChild, LibraryError> {
        // Windows install and recovery use this same lock to serialize filesystem/SQLite changes.
        let _lock = self.lock_download_slot()?;
        let candidate = self
            .launch_candidates(profile, release)?
            .into_iter()
            .find(|candidate| {
                candidate.association_id == association && candidate.entry_id == entry
            })
            .ok_or_else(|| LibraryError::Invalid("launch choice unavailable".into()))?;
        if candidate.declaration_revision != revision || candidate.status != "Ready" {
            return Err(LibraryError::Invalid(
                "launch choice changed or is no longer ready".into(),
            ));
        }
        let install = self
            .installation_for(profile, association, &candidate.artifact_id)?
            .ok_or_else(|| LibraryError::Invalid("installed runtime unavailable".into()))?;
        let target = candidate.target.as_deref().unwrap();
        let work = candidate.working_directory.as_deref().unwrap();
        let pinned = windows_fs::bind_launch_target(&install.path, &install.id, target, work)?;
        if self.conn.execute(
            "UPDATE installations SET process_id=NULL,process_start='windows:pending' WHERE id=?1 AND state='Ready'", [&install.id],
        )? != 1 {
            return Err(LibraryError::Invalid("installation is no longer ready".into()));
        }
        let initial = match create_suspended(
            &install.path.join(target),
            &install.path.join(work),
            &candidate.arguments,
        ) {
            Ok(process) => process,
            Err(error) => {
                self.conn.execute("UPDATE installations SET process_start=NULL WHERE id=?1 AND process_start='windows:pending'", [&install.id])?;
                return Err(error.into());
            }
        };
        let reopened = windows_fs::bind_launch_target(&install.path, &install.id, target, work)?;
        if !pinned.matches(&reopened)? {
            return Err(LibraryError::Invalid(
                "Windows launch path identity changed".into(),
            ));
        }
        let (pid, start) = initial.identity()?;
        if self.conn.execute(
            "UPDATE installations SET process_id=?2,process_start=?3 WHERE id=?1 AND process_start='windows:pending'",
            params![install.id, pid, format!("windows:{start}")],
        )? != 1 {
            return Err(LibraryError::Invalid("could not persist launch identity".into()));
        }
        Ok(initial.resume()?)
    }
}

fn process_state(pid: Option<i64>, time: Option<&str>) -> ProcessState {
    match (pid, time) {
        (None, None) => ProcessState::Idle,
        (Some(pid), Some(time)) => match (
            u32::try_from(pid),
            time.strip_prefix("windows:")
                .and_then(|time| time.parse().ok()),
        ) {
            (Ok(pid @ 1..), Some(time)) => match is_running(pid, time) {
                Ok(true) => ProcessState::Running,
                Ok(false) => ProcessState::Idle,
                Err(_) => ProcessState::Unknown,
            },
            _ => ProcessState::Unknown,
        },
        _ => ProcessState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_extensionless_child_has_stable_identity_before_running() {
        let cwd = std::env::current_dir().unwrap();
        if cwd.join("m41b1-child-test").is_file() {
            assert!(std::env::var_os("APP_DEPOT_FIXTURE_SECRET").is_none());
            std::fs::write(cwd.join("started"), b"native process").unwrap();
            std::process::exit(23);
        }

        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("payload");
        std::fs::copy(std::env::current_exe().unwrap(), &image).unwrap();
        std::fs::write(temp.path().join("m41b1-child-test"), b"fixture").unwrap();
        let suspended = create_suspended(
            &image,
            temp.path(),
            &["--exact".into(), "windows_process::tests::suspended_extensionless_child_has_stable_identity_before_running".into()],
        ).unwrap();
        let (pid, created) = suspended.identity().unwrap();
        assert!(is_running(pid, created).unwrap());
        assert!(!is_running(pid, created.wrapping_add(1)).unwrap());
        assert!(!temp.path().join("started").exists());
        let mut child = suspended.resume().unwrap();
        assert_eq!(child.id(), pid);
        assert_eq!(child.wait().unwrap().code(), Some(23));
        assert!(!is_running(pid, created).unwrap());
        assert_eq!(
            std::fs::read(temp.path().join("started")).unwrap(),
            b"native process"
        );
        assert_eq!(
            process_state(None, Some("windows:pending")),
            ProcessState::Unknown
        );
    }
}
