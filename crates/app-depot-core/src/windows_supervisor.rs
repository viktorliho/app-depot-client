//! One user-mode watcher per managed job. Only positive completion clears occupancy.
use crate::{
    library::{Library, LibraryPaths},
    windows_process::{ProcessState, Suspended, creation_time, is_running},
};
use rusqlite::params;
use std::{
    ffi::OsStr,
    io::{self, BufRead, BufReader, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Command, Stdio},
    ptr::null,
    sync::mpsc,
    thread,
    time::Duration,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND},
    System::Threading::CREATE_NO_WINDOW,
};

const ARG: &str = "--app-depot-internal-supervisor-v1";
const PREFIX: &str = "windows:sv1:";
const EXITED: &str = "windows:sv1:exited:";

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JobObjectBasicAccountingInformation, OpenJobObjectW, QueryInformationJobObject,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_QUERY;

fn job_name(id: uuid::Uuid) -> Vec<u16> {
    format!("Global\\AppDepot.Process.{id}")
        .encode_utf16()
        .chain([0])
        .collect()
}

pub(crate) fn new_job(id: uuid::Uuid, child: &Suspended) -> io::Result<OwnedHandle> {
    let name = job_name(id);
    // SAFETY: name is nul-terminated. Default security attributes do not inherit the handle.
    let raw = unsafe { CreateJobObjectW(null(), name.as_ptr()) };
    let status = io::Error::last_os_error();
    if raw.is_null() {
        return Err(status);
    }
    // SAFETY: CreateJobObjectW returned a fresh owned handle, even on an existing-name error.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    if status.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
        return Err(status);
    }
    let process = child.raw_process();
    // SAFETY: both handles remain live and refer to the job and our suspended child.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

fn open_job(id: uuid::Uuid) -> io::Result<Option<OwnedHandle>> {
    let name = job_name(id);
    // SAFETY: name is nul-terminated and the result is either null or an owned handle.
    let raw = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr()) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    // SAFETY: OpenJobObjectW returned a valid owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    Ok(Some(job))
}
pub(crate) fn job_active(id: uuid::Uuid) -> io::Result<Option<bool>> {
    open_job(id)?.as_ref().map(active).transpose()
}
fn active(job: &OwnedHandle) -> io::Result<bool> {
    let mut counts = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: the live job and output buffer match the information class.
    if unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicAccountingInformation,
            (&raw mut counts).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(counts.ActiveProcesses != 0)
}

pub(crate) fn prepare(
    paths: &LibraryPaths,
    installation: &str,
    child: &Suspended,
    started: u64,
    previous: Option<&str>,
) -> io::Result<(String, OwnedHandle)> {
    let nonce = Uuid::new_v4();
    let job = new_job(nonce, child)?;
    let untracked = previous.is_some_and(|value| !clean_exit(value));
    let mut process = Command::new(std::env::current_exe()?)
        .arg(ARG)
        .arg(&paths.metadata)
        .arg(&paths.content)
        .arg(installation)
        .arg(nonce.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let outcome = (|| {
        let time = creation_time(process.as_raw_handle())?;
        let pipe = process.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reply = String::new();
            let _ = tx.send(BufReader::new(pipe).read_line(&mut reply).map(|_| reply));
        });
        let reply = rx
            .recv_timeout(Duration::from_secs(8))
            .map_err(io::Error::other)??;
        if reply.trim_end() != nonce.to_string() {
            return Err(io::Error::other("supervisor did not acknowledge its job"));
        }
        Ok((
            format!(
                "{PREFIX}{started}:{nonce}:{}:{time}:{}",
                process.id(),
                u8::from(untracked)
            ),
            job,
        ))
    })();
    if outcome.is_err() {
        // Only this owned helper can be terminated here. The job has no kill-on-close flag.
        let _ = process.kill();
        let _ = process.wait();
    }
    outcome
}

pub(crate) fn dispatch() -> Option<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(ARG)) {
        return None;
    }
    Some(
        (|| {
            let paths = LibraryPaths {
                metadata: PathBuf::from(args.next()?),
                content: PathBuf::from(args.next()?),
            };
            let installation = args.next()?.into_string().ok()?;
            let job = Uuid::parse_str(&args.next()?.into_string().ok()?).ok()?;
            if args.next().is_some() || Uuid::parse_str(&installation).is_err() {
                return None;
            }
            supervise(paths, &installation, job).ok()
        })()
        .is_some(),
    )
}

fn supervise(paths: LibraryPaths, installation: &str, nonce: Uuid) -> io::Result<()> {
    let job = open_job(nonce)?.ok_or_else(|| io::Error::other("launch job is unavailable"))?;
    let library = Library::open(paths).map_err(io::Error::other)?;
    {
        let mut pipe = io::stdout().lock();
        writeln!(pipe, "{nonce}")?;
        pipe.flush()?;
    }
    while active(&job)? {
        thread::sleep(Duration::from_millis(250));
    }
    // SAFETY: this pseudo-handle refers to the current authenticated helper.
    let started =
        creation_time(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })?;
    let record: Option<String> = library
        .conn
        .query_row(
            "SELECT process_start FROM installations WHERE id=?1",
            [installation],
            |r| r.get(0),
        )
        .map_err(io::Error::other)?;
    if let Some(record) = record
        && let Some(launch) = live_record(&record)
        && launch.job == nonce
        && launch.supervisor == std::process::id()
        && launch.supervisor_started == started
    {
        library.conn.execute(
            "UPDATE installations SET process_id=NULL,process_start=?2 WHERE id=?1 AND process_start=?3",
            params![installation, format!("{EXITED}{nonce}:{}", u8::from(launch.untracked)), record],
        ).map_err(io::Error::other)?;
    }
    Ok(())
}

struct LiveRecord {
    job: Uuid,
    supervisor: u32,
    supervisor_started: u64,
    untracked: bool,
}

fn live_record(record: &str) -> Option<LiveRecord> {
    let mut fields = record.strip_prefix(PREFIX)?.split(':');
    fields.next()?.parse::<u64>().ok()?;
    let job = Uuid::parse_str(fields.next()?).ok()?;
    let supervisor = fields.next()?.parse().ok()?;
    let supervisor_started = fields.next()?.parse().ok()?;
    let untracked = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    (supervisor != 0 && fields.next().is_none()).then_some(LiveRecord {
        job,
        supervisor,
        supervisor_started,
        untracked,
    })
}

fn exited_record(record: &str) -> Option<bool> {
    let (nonce, flag) = record.strip_prefix(EXITED)?.split_once(':')?;
    Uuid::parse_str(nonce).ok()?;
    match flag {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn clean_exit(record: &str) -> bool {
    exited_record(record) == Some(false)
}

pub(crate) fn play_state(record: &str) -> Option<ProcessState> {
    if exited_record(record).is_some() {
        return Some(ProcessState::Idle);
    }
    if !record.starts_with(PREFIX) {
        return None;
    }
    Some(
        match live_record(record).and_then(|launch| {
            if is_running(launch.supervisor, launch.supervisor_started).ok()? {
                job_active(launch.job).ok().flatten()
            } else {
                None
            }
        }) {
            Some(true) => ProcessState::Running,
            _ => ProcessState::Unknown,
        },
    )
}

pub(crate) fn occupancy(record: Option<&str>, state: ProcessState) -> ProcessState {
    match (record, state) {
        (_, ProcessState::Running) => ProcessState::Running,
        (None, ProcessState::Idle) => ProcessState::Idle,
        (Some(record), ProcessState::Idle) if clean_exit(record) => ProcessState::Idle,
        _ => ProcessState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_supervisor_and_legacy_launch_cannot_release_occupancy() {
        let job = Uuid::new_v4();
        assert_eq!(
            play_state(&format!("{PREFIX}1:{job}:1:1:0")),
            Some(ProcessState::Unknown)
        );
        assert_eq!(
            occupancy(Some("windows:12"), ProcessState::Idle),
            ProcessState::Unknown
        );
        assert_eq!(
            occupancy(Some(&format!("{EXITED}{job}:1")), ProcessState::Idle),
            ProcessState::Unknown
        );
        assert_eq!(
            occupancy(Some(&format!("{EXITED}{job}:0")), ProcessState::Idle),
            ProcessState::Idle
        );
        assert_eq!(
            occupancy(Some("windows:pending"), ProcessState::Unknown),
            ProcessState::Unknown
        );
    }
}
