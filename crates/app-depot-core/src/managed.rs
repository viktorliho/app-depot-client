//! Managed writable copies of verified portable Artifacts. Paths use local UUIDs only.
use crate::{
    ServerProfileId,
    library::{Library, LibraryError},
};
#[cfg(target_os = "linux")]
use crate::{
    library::now,
    models::InventoryEntry,
    transfer::{hash_file, open_regular, private_dir, sync_dir},
    tree::{identity, validate_destination},
    tree_download::{create_file_beneath, dir_beneath, open_dir_nofollow, rename_tree_noreplace},
};
#[cfg(target_os = "linux")]
use fs2::FileExt;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::{collections::BTreeSet, fs::File, io::Read, os::fd::AsRawFd};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[cfg(target_os = "linux")]
fn statx_at(parent: &File, name: &std::ffi::CStr) -> Result<libc::statx, LibraryError> {
    let mut result: libc::statx = unsafe { std::mem::zeroed() };
    let code = unsafe {
        libc::statx(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
            &mut result,
        )
    };
    if code != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if result.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(invalid("filesystem cannot prove mount boundary"));
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn statx_handle(handle: &File) -> Result<libc::statx, LibraryError> {
    let empty = std::ffi::CString::new("").expect("literal");
    let mut result: libc::statx = unsafe { std::mem::zeroed() };
    let code = unsafe {
        libc::statx(
            handle.as_raw_fd(),
            empty.as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
            &mut result,
        )
    };
    if code != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if result.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(invalid("filesystem cannot prove mount boundary"));
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn delete_entry(parent: &File, name: &std::ffi::OsStr, mount: u64) -> Result<(), LibraryError> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name =
        std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid("NUL in managed path"))?;
    let before = match statx_at(parent, &name) {
        Ok(v) => v,
        Err(LibraryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if before.stx_mnt_id != mount {
        return Err(invalid("nested mount inside managed resource"));
    }
    if u32::from(before.stx_mode) & libc::S_IFMT == libc::S_IFDIR {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let child = unsafe { File::from_raw_fd(fd) };
        let opened = statx_handle(&child)?;
        if opened.stx_mnt_id != mount || opened.stx_ino != before.stx_ino {
            return Err(invalid("managed directory changed during removal"));
        }
        for item in fs::read_dir(format!("/proc/self/fd/{}", child.as_raw_fd()))? {
            let item = item?;
            delete_entry(&child, &item.file_name(), mount)?;
        }
        child.sync_all()?;
        let after = statx_at(parent, &name)?;
        if after.stx_mnt_id != mount || after.stx_ino != opened.stx_ino {
            return Err(invalid("managed directory changed during removal"));
        }
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    } else {
        // unlinkat removes a symlink itself; it never follows its target.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    parent.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_owned_mounts(
    parent: &File,
    name: &std::ffi::OsStr,
    mount: u64,
) -> Result<(), LibraryError> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name =
        std::ffi::CString::new(name.as_bytes()).map_err(|_| invalid("NUL in managed path"))?;
    let entry = statx_at(parent, &name)?;
    if entry.stx_mnt_id != mount {
        return Err(invalid("nested mount inside managed resource"));
    }
    if u32::from(entry.stx_mode) & libc::S_IFMT == libc::S_IFDIR {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let directory = unsafe { File::from_raw_fd(fd) };
        let opened = statx_handle(&directory)?;
        if opened.stx_mnt_id != mount || opened.stx_ino != entry.stx_ino {
            return Err(invalid("managed directory changed during validation"));
        }
        for child in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
            validate_owned_mounts(&directory, &child?.file_name(), mount)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn delete_owned(root: &Path, parent: &Path, name: &str) -> Result<(), LibraryError> {
    use std::ffi::OsStr;
    if name.contains('/') || name.is_empty() || name == "." || name == ".." {
        return Err(invalid("unsafe managed child name"));
    }
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| invalid("managed deletion is outside the library"))?;
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(invalid("unsafe managed parent path"));
    }
    let root_handle = crate::launch::open_nofollow(root, "", true)?;
    let directory = crate::launch::open_nofollow(parent, "", true)?;
    let mount = statx_handle(&root_handle)?.stx_mnt_id;
    if statx_handle(&directory)?.stx_mnt_id != mount {
        return Err(invalid("managed deletion crosses a mount boundary"));
    }
    delete_entry(&directory, OsStr::new(name), mount)
}

#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Result<Option<String>, LibraryError> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(v) => v,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let tail = stat
        .rsplit_once(") ")
        .ok_or_else(|| invalid("malformed process status"))?
        .1;
    let start = tail
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| invalid("missing process start time"))?;
    Ok(Some(start.to_owned()))
}

#[cfg(target_os = "linux")]
fn runtime_busy(path: &Path, pid: Option<i64>, start: Option<&str>) -> Result<bool, LibraryError> {
    use std::os::unix::fs::MetadataExt;
    if let (Some(pid), Some(start)) = (pid, start) {
        if pid <= 0 || pid > u32::MAX as i64 {
            return Err(invalid("invalid recorded process ID"));
        }
        match process_start(pid as u32) {
            Ok(Some(actual)) if actual == start => return Ok(true),
            Ok(_) => {}
            Err(_) => return Err(invalid("cannot verify recorded application process")),
        }
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let uid = unsafe { libc::geteuid() };
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let process = entry.path();
        let Ok(metadata) = fs::metadata(&process) else {
            continue;
        };
        if metadata.uid() != uid {
            continue;
        }
        for name in ["exe", "cwd"] {
            match fs::read_link(process.join(name)) {
                Ok(target) if target.starts_with(path) => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(false)
}

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub id: String,
    pub profile_id: ServerProfileId,
    pub app_id: String,
    pub release_id: String,
    pub association_id: String,
    pub artifact_id: String,
    pub state: String,
    pub path: PathBuf,
    pub userdata: PathBuf,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationIntegrity {
    pub installation_id: String,
    pub missing: Vec<String>,
    pub modified: Vec<String>,
    pub unexpected: Vec<String>,
}

impl InstallationIntegrity {
    pub fn matches_original(&self) -> bool {
        self.missing.is_empty() && self.modified.is_empty() && self.unexpected.is_empty()
    }
}

#[cfg(target_os = "linux")]
fn hash_open(mut file: File) -> Result<String, LibraryError> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(target_os = "linux")]
fn runtime_paths(root: &Path) -> Result<BTreeSet<String>, LibraryError> {
    fn visit(
        root: &Path,
        relative: &str,
        mount: u64,
        paths: &mut BTreeSet<String>,
    ) -> Result<(), LibraryError> {
        let dir = crate::launch::open_nofollow(root, relative, true)?;
        if statx_handle(&dir)?.stx_mnt_id != mount {
            return Err(invalid("runtime contains a nested mount"));
        }
        for item in fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
            let item = item?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| invalid("runtime contains a non-UTF-8 name"))?;
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            paths.insert(path.clone());
            if item.file_type()?.is_dir() {
                visit(root, &path, mount, paths)?;
            }
        }
        Ok(())
    }
    let handle = crate::launch::open_nofollow(root, "", true)?;
    let mut paths = BTreeSet::new();
    visit(root, "", statx_handle(&handle)?.stx_mnt_id, &mut paths)?;
    Ok(paths)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    pub immutable_objects_logical_bytes: u64,
    pub verified_materializations_logical_bytes: u64,
    pub writable_installations_logical_bytes: u64,
    pub managed_userdata_logical_bytes: u64,
    pub staging_logical_bytes: u64,
    pub artwork_logical_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalPreview {
    pub installation: Installation,
    pub complete: bool,
    pub working_copy_logical_bytes: u64,
    pub managed_data_logical_bytes: u64,
    pub verified_materialization_logical_bytes: u64,
    pub other_verified_references: u64,
}

fn owned_logical_bytes(path: &Path) -> Result<u64, LibraryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(0),
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(metadata) if metadata.is_dir() => directory_logical_bytes(path),
        Ok(_) => Err(invalid("unsupported managed entry")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn directory_logical_bytes(root: &Path) -> Result<u64, LibraryError> {
    let mut sum = 0u64;
    for item in fs::read_dir(root)? {
        let item = item?;
        let metadata = fs::symlink_metadata(item.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let amount = if metadata.is_dir() {
            directory_logical_bytes(&item.path())?
        } else if metadata.is_file() {
            metadata.len()
        } else {
            0
        };
        sum = sum
            .checked_add(amount)
            .ok_or_else(|| invalid("storage usage overflow"))?;
    }
    Ok(sum)
}

fn safe_id(id: &str) -> Result<(), LibraryError> {
    Uuid::parse_str(id).map_err(|_| invalid("invalid managed installation ID"))?;
    if id.len() != 36 || id.to_ascii_lowercase() != id {
        return Err(invalid("noncanonical managed installation ID"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn local_artifact_digest(id: &str) -> Result<&str, LibraryError> {
    let digest = id
        .strip_prefix("adfile:sha256:")
        .or_else(|| id.strip_prefix("adtree:v1:sha256:"))
        .ok_or_else(|| invalid("invalid Artifact identity"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(invalid("invalid Artifact digest"));
    }
    Ok(digest)
}

#[cfg(target_os = "linux")]
type UninstallTarget = (String, String, String, Option<String>);
#[cfg(target_os = "linux")]
pub struct LocalUninstall {
    pub revision: String,
    pub states: Vec<String>,
    pub runtime_bytes: u64,
    pub download_bytes: u64,
    pub blocked: bool,
}

#[cfg(target_os = "linux")]
fn copy_independent(
    source: &Path,
    destination: &mut File,
    destination_path: &Path,
    size: u64,
    digest: &str,
    executable: bool,
) -> Result<(), LibraryError> {
    use std::os::unix::fs::PermissionsExt;
    let mut input = open_regular(source, false)?;
    if hash_file(source, size)? != digest {
        return Err(invalid("immutable object failed SHA-256 validation"));
    }
    // FICLONE creates independent inodes with shared COW blocks. Unsupported filesystems
    // transparently fall back to a bounded ordinary copy.
    let cloned =
        unsafe { libc::ioctl(destination.as_raw_fd(), 0x4004_9409_u64, input.as_raw_fd()) } == 0;
    if !cloned {
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP | libc::EXDEV | libc::ENOTTY | libc::EINVAL)
        ) {
            return Err(error.into());
        }
        std::io::copy(&mut input, destination)?;
    }
    destination.set_permissions(fs::Permissions::from_mode(if executable {
        0o700
    } else {
        0o600
    }))?;
    destination.sync_all()?;
    if hash_file(destination_path, size)? != digest {
        return Err(invalid("runtime copy failed SHA-256 validation"));
    }
    Ok(())
}

impl Library {
    /// Logical byte counts only. Reflink sharing and filesystem compression prevent a
    /// reliable per-resource physical reclaim estimate without filesystem-specific APIs.
    pub fn storage_usage(&self) -> Result<StorageUsage, LibraryError> {
        let root = &self.paths.content;
        Ok(StorageUsage {
            immutable_objects_logical_bytes: directory_logical_bytes(&root.join("objects"))?,
            verified_materializations_logical_bytes: directory_logical_bytes(
                &root.join("artifacts"),
            )?,
            writable_installations_logical_bytes: directory_logical_bytes(&root.join("apps"))?,
            managed_userdata_logical_bytes: directory_logical_bytes(&root.join("userdata"))?,
            staging_logical_bytes: directory_logical_bytes(&root.join("staging"))?,
            artwork_logical_bytes: directory_logical_bytes(&root.join("artwork"))?,
        })
    }
    #[cfg(target_os = "linux")]
    fn uninstall_targets(
        &self,
        profile: &ServerProfileId,
        app: &str,
    ) -> Result<Vec<UninstallTarget>, LibraryError> {
        let mut stmt = self.conn.prepare(
            "WITH owned AS (
                 SELECT association_id,artifact_id,release_id FROM installations
                    WHERE profile_id=?1 AND app_id=?2
                 UNION
                 SELECT j.association_id,j.artifact_id,r.id FROM download_jobs j
                    JOIN associations a ON (a.profile_id,a.id)=(j.profile_id,j.association_id)
                    JOIN releases r ON (r.profile_id,r.id)=(a.profile_id,a.release_id)
                    WHERE j.profile_id=?1 AND r.app_id=?2
                 UNION
                 SELECT v.association_id,v.artifact_id,r.id FROM verified_content v
                    JOIN associations a ON (a.profile_id,a.id)=(v.profile_id,v.association_id)
                    JOIN releases r ON (r.profile_id,r.id)=(a.profile_id,a.release_id)
                    WHERE v.profile_id=?1 AND r.app_id=?2
             ) SELECT o.association_id,o.artifact_id,o.release_id,i.id FROM owned o
                 LEFT JOIN installations i ON (i.profile_id,i.association_id,i.artifact_id)=(?1,o.association_id,o.artifact_id)
                 ORDER BY o.association_id,o.artifact_id")?;
        Ok(stmt
            .query_map(params![profile.0, app], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?)
    }

    #[cfg(target_os = "linux")]
    fn inspect_uninstall(
        &self,
        profile: &ServerProfileId,
        targets: &[UninstallTarget],
    ) -> Result<Option<LocalUninstall>, LibraryError> {
        let mut revision = Sha256::new();
        let mut states = BTreeSet::new();
        let mut runtime_bytes = 0u64;
        let mut download_bytes = 0u64;
        let mut blocked = false;
        let mut local = false;
        for (association, artifact, release, id) in targets {
            revision.update(serde_json::to_vec(&(association, artifact, release, id))?);
            if let Some(id) = id {
                let item = self.installation(id)?;
                let (updated, pid, start): (i64, Option<i64>, Option<String>) =
                    self.conn.query_row(
                        "SELECT updated_at,process_id,process_start FROM installations WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?;
                revision.update(serde_json::to_vec(&(&item.state, updated, pid, &start))?);
                blocked |=
                    item.state == "Preparing" || runtime_busy(&item.path, pid, start.as_deref())?;
                if item.state != "Removed" || owned_logical_bytes(&item.userdata)? > 0 {
                    local = true;
                    states.insert(item.state);
                }
                runtime_bytes = runtime_bytes
                    .saturating_add(owned_logical_bytes(&item.path)?)
                    .saturating_add(owned_logical_bytes(&item.userdata)?);
            }
            let mut jobs = self.conn.prepare("SELECT id,state,COALESCE(expected_size,0),COALESCE(updated_at,created_at) FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 ORDER BY id")?;
            for job in jobs.query_map(params![profile.0, association, artifact], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })? {
                let job = job?;
                revision.update(serde_json::to_vec(&job)?);
                blocked |= matches!(
                    job.1.as_str(),
                    "Queued" | "FetchingInventory" | "Downloading" | "Verifying" | "Committing"
                );
            }
            let verified: i64 = self.conn.query_row(
                "SELECT COALESCE((SELECT MAX(expected_size) FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Completed'),0)
                    FROM verified_content WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 LIMIT 1",
                params![profile.0,association,artifact], |r| r.get(0)).optional()?.unwrap_or(-1);
            revision.update(verified.to_le_bytes());
            if let Ok(size) = u64::try_from(verified) {
                local = true;
                download_bytes = download_bytes.saturating_add(size);
            }
        }
        Ok(local.then(|| LocalUninstall {
            revision: format!("{:x}", revision.finalize()),
            states: states.into_iter().collect(),
            runtime_bytes,
            download_bytes,
            blocked,
        }))
    }

    #[cfg(target_os = "linux")]
    pub fn local_uninstall_preview(
        &self,
        profile: &ServerProfileId,
        app: &str,
    ) -> Result<Option<LocalUninstall>, LibraryError> {
        let mut preview =
            self.inspect_uninstall(profile, &self.uninstall_targets(profile, app)?)?;
        if let Some(preview) = &mut preview {
            for lock in [self.lifecycle_lock(), self.lock_download_slot()] {
                match lock {
                    Ok(_) => {}
                    Err(LibraryError::Invalid(message))
                        if message == "another library lifecycle operation is active"
                            || message == "another application download holds the library slot" =>
                    {
                        preview.blocked = true
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(preview)
    }

    /// Journal every selected Association before the first destructive operation.
    #[cfg(target_os = "linux")]
    pub fn uninstall_local_app(
        &mut self,
        profile: &ServerProfileId,
        app: &str,
        revision: &str,
    ) -> Result<bool, LibraryError> {
        let _lifecycle = self.lifecycle_lock()?;
        let _download = self.lock_download_slot()?;
        let ids = self.journal_uninstall(profile, app, revision)?;
        for id in ids {
            self.remove_installation_locked(&id, true, false, &mut |_| {})?;
        }
        Ok(self.collect_unreferenced_objects().is_ok())
    }

    #[cfg(target_os = "linux")]
    fn journal_uninstall(
        &self,
        profile: &ServerProfileId,
        app: &str,
        revision: &str,
    ) -> Result<BTreeSet<String>, LibraryError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let targets = self.uninstall_targets(profile, app)?;
        let preview = self
            .inspect_uninstall(profile, &targets)?
            .ok_or_else(|| invalid("local uninstall selection changed"))?;
        if preview.blocked {
            return Err(invalid("local uninstall blocked by an active operation"));
        }
        if preview.revision != revision {
            return Err(invalid("local uninstall selection changed"));
        }
        self.conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS block_uninstall_enqueue BEFORE INSERT ON download_jobs
             WHEN EXISTS (SELECT 1 FROM installations i WHERE i.profile_id=NEW.profile_id
               AND i.association_id=NEW.association_id AND i.state='Removing' AND i.remove_complete=1)
             BEGIN SELECT RAISE(ABORT,'Association is being uninstalled'); END;
             CREATE TRIGGER IF NOT EXISTS block_uninstall_requeue BEFORE UPDATE OF state ON download_jobs
             WHEN NEW.state='Queued' AND EXISTS (SELECT 1 FROM installations i WHERE i.profile_id=NEW.profile_id
               AND i.association_id=NEW.association_id AND i.state='Removing' AND i.remove_complete=1)
             BEGIN SELECT RAISE(ABORT,'Association is being uninstalled'); END;",
        )?;
        let mut ids = BTreeSet::new();
        for (association, artifact, release, id) in targets {
            safe_id(&profile.0)?;
            safe_id(&association)?;
            local_artifact_digest(&artifact)?;
            let owner: String = self.conn.query_row(
                "SELECT r.app_id FROM associations a JOIN releases r ON (a.profile_id,a.release_id)=(r.profile_id,r.id) WHERE a.profile_id=?1 AND a.id=?2",
                params![profile.0,association], |row| row.get(0))?;
            if owner != app {
                return Err(invalid("local uninstall association owner changed"));
            }
            let id = if let Some(id) = id {
                if self.installation(&id)?.app_id != app {
                    return Err(invalid("local uninstall association owner changed"));
                }
                self.conn.execute("UPDATE installations SET state='Removing',remove_complete=1,updated_at=?2 WHERE id=?1",params![id,now()?])?;
                id
            } else {
                let id = Uuid::new_v4().to_string();
                self.conn.execute("INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at,remove_complete) VALUES (?1,?2,?3,?4,?5,?6,'Removing',?7,?7,1)", params![id,profile.0,app,release,association,artifact,now()?])?;
                id
            };
            ids.insert(id);
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Remove an explicitly selected local Association reference, including a data-only
    /// download with no portable runtime. The existing installation row is also the
    /// durable removal journal; no Server resource is changed.
    #[cfg(target_os = "linux")]
    pub fn remove_local_artifact(
        &mut self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<(), LibraryError> {
        let (release,app,artifact):(String,String,String)=self.conn.query_row("SELECT a.release_id,r.app_id,a.artifact_id FROM associations a JOIN releases r ON r.profile_id=a.profile_id AND r.id=a.release_id WHERE a.profile_id=?1 AND a.id=?2",params![profile.0,association],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if let Some(item) = self.installation_for(profile, association, &artifact)? {
            return self.remove_installation(&item.id, true);
        }
        let active:i64=self.conn.query_row("SELECT count(*) FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND state IN ('Queued','FetchingInventory','Downloading','Verifying','Committing')",params![profile.0,association],|r|r.get(0))?;
        if active != 0 {
            return Err(invalid(
                "pause or cancel the active download before removal",
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.conn.execute("INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at,remove_complete) VALUES (?1,?2,?3,?4,?5,?6,'Removing',?7,?7,1)",params![id,profile.0,app,release,association,artifact,now()?])?;
        self.remove_installation(&id, true)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn remove_local_artifact(
        &mut self,
        _profile: &ServerProfileId,
        _association: &str,
    ) -> Result<(), LibraryError> {
        Err(invalid(
            "managed removal is currently available only on Linux",
        ))
    }

    /// Remove the selected App's local associations only. Cached catalog and Server
    /// profile identity remain; shared objects survive while another owner exists.
    #[cfg(target_os = "linux")]
    pub fn remove_local_app(
        &mut self,
        profile: &ServerProfileId,
        app: &str,
        complete: bool,
    ) -> Result<usize, LibraryError> {
        self.remove_local_app_observed(profile, app, complete, &mut |_, _| {})
    }

    #[cfg(target_os = "linux")]
    pub fn remove_local_app_observed(
        &mut self,
        profile: &ServerProfileId,
        app: &str,
        complete: bool,
        observer: &mut dyn FnMut(usize, usize),
    ) -> Result<usize, LibraryError> {
        let associations: Vec<(String, String)> = {
            let mut stmt=self.conn.prepare("SELECT a.id,a.artifact_id FROM associations a JOIN releases r ON r.profile_id=a.profile_id AND r.id=a.release_id WHERE r.profile_id=?1 AND r.app_id=?2 ORDER BY a.id")?;
            stmt.query_map(params![profile.0, app], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        let mut removed = 0;
        let total = associations.len();
        for (index, (association, artifact)) in associations.into_iter().enumerate() {
            observer(index + 1, total);
            if complete {
                let owned:i64=self.conn.query_row("SELECT (SELECT count(*) FROM verified_content WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3)+(SELECT count(*) FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3)",params![profile.0,association,artifact],|r|r.get(0))?;
                if owned != 0
                    || self
                        .installation_for(profile, &association, &artifact)?
                        .is_some()
                {
                    self.remove_local_artifact(profile, &association)?;
                    removed += 1;
                }
            } else if let Some(item) = self.installation_for(profile, &association, &artifact)?
                && item.state != "Removed"
            {
                self.remove_installation(&item.id, false)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn remove_local_app(
        &mut self,
        _profile: &ServerProfileId,
        _app: &str,
        _complete: bool,
    ) -> Result<usize, LibraryError> {
        Err(invalid(
            "managed removal is currently available only on Linux",
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn remove_local_app_observed(
        &mut self,
        _profile: &ServerProfileId,
        _app: &str,
        _complete: bool,
        _observer: &mut dyn FnMut(usize, usize),
    ) -> Result<usize, LibraryError> {
        Err(invalid(
            "managed removal is currently available only on Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    fn lifecycle_lock(&self) -> Result<File, LibraryError> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.paths.content.join(".app-depot-lifecycle.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("unsafe lifecycle lock"));
        }
        file.try_lock_exclusive()
            .map_err(|_| invalid("another library lifecycle operation is active"))?;
        Ok(file)
    }

    /// Reassess the cached declaration while holding the same local lifecycle lock used
    /// by runtime preparation and removal. The returned child is owned by the caller.
    #[cfg(target_os = "linux")]
    pub fn play_declared(
        &self,
        profile: &ServerProfileId,
        release: &str,
        association: &str,
        entry: &str,
        revision: &str,
    ) -> Result<std::process::Child, LibraryError> {
        let _lock = self.lifecycle_lock()?;
        let candidate = self
            .launch_candidates(profile, release)?
            .into_iter()
            .find(|c| c.association_id == association && c.entry_id == entry)
            .ok_or_else(|| invalid("launch choice unavailable"))?;
        if candidate.declaration_revision != revision {
            return Err(invalid(
                "launch declaration changed; review the choice again",
            ));
        }
        let installation = self
            .installation_for(profile, association, &candidate.artifact_id)?
            .ok_or_else(|| invalid("managed runtime unavailable"))?;
        let tracked: (Option<i64>, Option<String>) = self.conn.query_row(
            "SELECT process_id,process_start FROM installations WHERE id=?1",
            [&installation.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if runtime_busy(&installation.path, tracked.0, tracked.1.as_deref())? {
            return Err(invalid(
                "application is already running from this managed runtime",
            ));
        }
        let mut child = candidate.spawn()?;
        let start = match process_start(child.id()) {
            Ok(Some(start)) => start,
            Ok(None) => String::new(),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = self.conn.execute(
            "UPDATE installations SET process_id=?2,process_start=?3 WHERE id=?1",
            params![installation.id, i64::from(child.id()), start],
        ) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok(child)
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn play_declared(
        &self,
        _profile: &ServerProfileId,
        _release: &str,
        _association: &str,
        _entry: &str,
        _revision: &str,
    ) -> Result<std::process::Child, LibraryError> {
        Err(invalid(
            "native portable launch is currently available only on Linux",
        ))
    }

    /// Reclaim only self-identifying immutable file objects with no durable owner.
    /// A SQLite write transaction prevents a new local reference during each decision.
    #[cfg(target_os = "linux")]
    pub fn collect_unreferenced_objects(&mut self) -> Result<u64, LibraryError> {
        // A damaged Tree member index cannot be used as negative proof of ownership.
        // Refuse sweeping until the durable inventory and member references agree.
        let completed: Vec<(String, String, String)> = {
            let mut stmt=self.conn.prepare("SELECT id,artifact_id,inventory_json FROM download_jobs WHERE artifact_id LIKE 'adtree:v1:sha256:%' AND state='Completed'")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?
        };
        for (job, artifact, raw) in completed {
            let entries: Vec<InventoryEntry> = serde_json::from_str(&raw)?;
            if identity(&entries)? != artifact {
                return Err(invalid(
                    "Tree inventory is inconsistent; object cleanup refused",
                ));
            }
            let recorded: i64 = self.conn.query_row(
                "SELECT count(*) FROM tree_files WHERE job_id=?1",
                [&job],
                |r| r.get(0),
            )?;
            let expected = entries
                .iter()
                .filter(|e| matches!(e, InventoryEntry::File { .. }))
                .count();
            if recorded != expected as i64 {
                return Err(invalid(
                    "Tree member index is incomplete; object cleanup refused",
                ));
            }
            for entry in entries {
                if let InventoryEntry::File { path, sha256, .. } = entry {
                    let actual: Option<String> = self
                        .conn
                        .query_row(
                            "SELECT sha256 FROM tree_files WHERE job_id=?1 AND path=?2",
                            params![job, path],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if actual.as_deref() != Some(sha256.as_str()) {
                        return Err(invalid(
                            "Tree member identity differs from inventory; object cleanup refused",
                        ));
                    }
                }
            }
        }
        let root = self.paths.content.join("objects");
        let mut objects = Vec::new();
        for shard in fs::read_dir(&root)? {
            let shard = shard?;
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if shard_name.len() != 2
                || !shard_name
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(invalid("unexpected object-store shard"));
            }
            crate::transfer::existing_dir(&shard.path())?;
            for object in fs::read_dir(shard.path())? {
                let object = object?;
                let digest = object.file_name().to_string_lossy().into_owned();
                if digest.len() != 64
                    || !digest.starts_with(&shard_name)
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(invalid("unexpected object-store entry"));
                }
                objects.push((shard.path(), digest));
            }
        }
        let mut reclaimed = 0u64;
        for (shard, digest) in objects {
            let tx = self.conn.transaction()?;
            let artifact = format!("adfile:sha256:{digest}");
            let refs:i64=tx.query_row("SELECT (SELECT count(*) FROM verified_content WHERE artifact_id=?1)+(SELECT count(*) FROM download_jobs WHERE artifact_id=?1)+(SELECT count(*) FROM tree_files WHERE sha256=?2)",params![artifact,digest],|r|r.get(0))?;
            if refs == 0 {
                let path = shard.join(&digest);
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(invalid("unsafe immutable object entry"));
                }
                if hash_file(&path, metadata.len())? != digest {
                    return Err(invalid(
                        "unreferenced object is corrupted; refusing cleanup",
                    ));
                }
                delete_owned(&self.paths.content, &shard, &digest)?;
                reclaimed = reclaimed
                    .checked_add(metadata.len())
                    .ok_or_else(|| invalid("reclaimed size overflow"))?;
            }
            tx.commit()?;
        }
        Ok(reclaimed)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn collect_unreferenced_objects(&mut self) -> Result<u64, LibraryError> {
        Err(invalid(
            "object cleanup is currently available only on Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn recover_removals(&mut self) -> Result<(), LibraryError> {
        match self.lifecycle_lock() {
            Ok(lock) => drop(lock),
            Err(LibraryError::Invalid(message))
                if message == "another library lifecycle operation is active" =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        let rows: Vec<(String, bool)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id,remove_complete FROM installations WHERE state='Removing'")?;
            stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
            })?
            .collect::<Result<_, _>>()?
        };
        for (id, complete) in rows {
            if let Err(error) = self.remove_installation(&id, complete) {
                if matches!(&error,LibraryError::Invalid(message) if message=="another library lifecycle operation is active")
                {
                    continue;
                }
                self.conn.execute(
                    "UPDATE installations SET last_error=?2,updated_at=?3 WHERE id=?1",
                    params![id, error.to_string(), now()?],
                )?;
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn recover_removals(&mut self) -> Result<(), LibraryError> {
        Ok(())
    }
    /// Remove one locally assigned runtime. Complete removal additionally drops this
    /// Association's local download reference and managed user data. The Server is untouched.
    /// A durable Removing record makes interruption retryable.
    #[cfg(target_os = "linux")]
    pub fn remove_installation(&mut self, id: &str, complete: bool) -> Result<(), LibraryError> {
        self.remove_installation_observed(id, complete, &mut |_| {})
    }

    #[cfg(target_os = "linux")]
    pub fn remove_installation_observed(
        &mut self,
        id: &str,
        complete: bool,
        observer: &mut dyn FnMut(&str),
    ) -> Result<(), LibraryError> {
        let _lock = self.lifecycle_lock()?;
        let _download = complete.then(|| self.lock_download_slot()).transpose()?;
        self.remove_installation_locked(id, complete, complete, observer)
    }

    #[cfg(target_os = "linux")]
    fn remove_installation_locked(
        &mut self,
        id: &str,
        complete: bool,
        collect: bool,
        observer: &mut dyn FnMut(&str),
    ) -> Result<(), LibraryError> {
        observer("Checking active processes");
        let item = self.installation(id)?;
        let tracked: (Option<i64>, Option<String>) = self.conn.query_row(
            "SELECT process_id,process_start FROM installations WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if runtime_busy(&item.path, tracked.0, tracked.1.as_deref())? {
            return Err(invalid("application is running from this managed runtime"));
        }
        let active:i64=self.conn.query_row("SELECT count(*) FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND state IN ('Queued','FetchingInventory','Downloading','Verifying','Committing')",params![item.profile_id.0,item.association_id],|r|r.get(0))?;
        if active != 0 {
            return Err(invalid("a download is active for this Association"));
        }
        if item.state == "Removing" {
            if complete
                != (self.conn.query_row(
                    "SELECT remove_complete FROM installations WHERE id=?1",
                    [id],
                    |r| r.get::<_, i64>(0),
                )? != 0)
            {
                return Err(invalid("removal mode differs from interrupted operation"));
            }
        } else if item.state == "Removed" && !complete {
            return Ok(());
        } else {
            self.conn.execute("UPDATE installations SET state='Removing',remove_complete=?2,updated_at=?3 WHERE id=?1",params![id,i64::from(complete),now()?])?;
        }
        observer("Removing working data");
        delete_owned(&self.paths.content, &self.paths.content.join("apps"), id)?;
        delete_owned(
            &self.paths.content,
            &self.paths.content.join("staging"),
            &format!("runtime-{id}"),
        )?;
        delete_owned(
            &self.paths.content,
            &self.paths.content.join("staging"),
            &format!("runtime-quarantine-{id}"),
        )?;
        if complete {
            observer("Removing local verified references");
            delete_owned(
                &self.paths.content,
                &self.paths.content.join("userdata"),
                id,
            )?;
            // Remove only paths derived from validated local identity components.
            safe_id(&item.profile_id.0)?;
            safe_id(&item.association_id)?;
            let digest = local_artifact_digest(&item.artifact_id)?;
            let association_dir = self
                .paths
                .content
                .join("artifacts")
                .join(&item.profile_id.0)
                .join(&item.association_id);
            if fs::symlink_metadata(&association_dir).is_ok() {
                delete_owned(&self.paths.content, &association_dir, digest)?;
            }
            let jobs: Vec<String> = {
                let mut stmt=self.conn.prepare("SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3")?;
                stmt.query_map(
                    params![item.profile_id.0, item.association_id, item.artifact_id],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<Result<_, _>>()?
            };
            for job in &jobs {
                safe_id(job)?;
                delete_owned(
                    &self.paths.content,
                    &self.paths.content.join("staging"),
                    job,
                )?;
                delete_owned(
                    &self.paths.content,
                    &self.paths.content.join("staging"),
                    &format!("source-repair-{job}"),
                )?;
                delete_owned(
                    &self.paths.content,
                    &self.paths.content.join("staging"),
                    &format!("source-quarantine-{job}"),
                )?;
            }
            let tx = self.conn.transaction()?;
            for job in &jobs {
                tx.execute("DELETE FROM tree_files WHERE job_id=?1", [job])?;
                tx.execute("DELETE FROM download_jobs WHERE id=?1", [job])?;
            }
            tx.execute("DELETE FROM verified_content WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3",params![item.profile_id.0,item.association_id,item.artifact_id])?;
            tx.execute(
                "DELETE FROM installations WHERE id=?1 AND state='Removing'",
                [id],
            )?;
            tx.commit()?;
            if collect {
                observer("Reclaiming unreferenced content");
                self.collect_unreferenced_objects()?;
            }
        } else {
            observer("Finalizing removal");
            self.conn.execute("UPDATE installations SET state='Removed',updated_at=?2,last_error=NULL WHERE id=?1 AND state='Removing'",params![id,now()?])?;
        }
        Ok(())
    }

    #[cfg(windows)]
    pub fn remove_installation(&mut self, id: &str, complete: bool) -> Result<(), LibraryError> {
        if complete {
            return Err(invalid("complete Windows removal is unavailable"));
        }
        self.remove_windows_runtime(id)
    }

    #[cfg(windows)]
    pub fn remove_installation_observed(
        &mut self,
        id: &str,
        complete: bool,
        observer: &mut dyn FnMut(&str),
    ) -> Result<(), LibraryError> {
        observer("Checking installation and removing owned files");
        self.remove_installation(id, complete)
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn remove_installation(&mut self, _id: &str, _complete: bool) -> Result<(), LibraryError> {
        Err(invalid(
            "managed removal is currently available only on Linux",
        ))
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn remove_installation_observed(
        &mut self,
        _id: &str,
        _complete: bool,
        _observer: &mut dyn FnMut(&str),
    ) -> Result<(), LibraryError> {
        Err(invalid(
            "managed removal is currently available only on Linux",
        ))
    }

    pub fn installations(
        &self,
        profile: Option<&ServerProfileId>,
    ) -> Result<Vec<Installation>, LibraryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM installations WHERE ?1 IS NULL OR profile_id=?1 ORDER BY created_at,id",
        )?;
        let ids = stmt
            .query_map([profile.map(|p| p.0.as_str())], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter().map(|id| self.installation(id)).collect()
    }

    pub fn installation(&self, id: &str) -> Result<Installation, LibraryError> {
        safe_id(id)?;
        let fields = self.conn.query_row(
            "SELECT profile_id,app_id,release_id,association_id,artifact_id,state,last_error FROM installations WHERE id=?1",
            [id], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?,r.get::<_, String>(2)?,r.get::<_, String>(3)?,r.get::<_, String>(4)?,r.get::<_, String>(5)?,r.get::<_, Option<String>>(6)?))
        ).optional()?.ok_or_else(|| invalid("installation not found"))?;
        Ok(Installation {
            id: id.into(),
            profile_id: ServerProfileId(fields.0),
            app_id: fields.1,
            release_id: fields.2,
            association_id: fields.3,
            artifact_id: fields.4,
            state: fields.5,
            path: self.paths.content.join("apps").join(id),
            userdata: self.paths.content.join("userdata").join(id),
            last_error: fields.6,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn verify_installation(&self, id: &str) -> Result<InstallationIntegrity, LibraryError> {
        let item = self.installation(id)?;
        if item.state != "Ready" {
            return Err(invalid("installation is not ready"));
        }
        let mut expected = BTreeSet::new();
        let mut files = Vec::new();
        let mut directories = Vec::new();
        if let Some(digest) = item.artifact_id.strip_prefix("adfile:sha256:") {
            let size = self
                .cached_artifact(&item.profile_id, &item.artifact_id)?
                .and_then(|value| value.value.size)
                .ok_or_else(|| invalid("cached File size missing"))?;
            expected.insert("payload".to_owned());
            files.push(("payload".to_owned(), size, digest.to_owned()));
        } else {
            let completed:String=self.conn.query_row("SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Completed' ORDER BY updated_at DESC LIMIT 1",params![item.profile_id.0,item.association_id,item.artifact_id],|r|r.get(0))?;
            let inventory = self
                .tree_inventory(&completed)?
                .ok_or_else(|| invalid("verified Tree inventory missing"))?;
            if identity(&inventory)? != item.artifact_id {
                return Err(invalid("verified Tree identity mismatch"));
            }
            for entry in inventory {
                expected.insert(entry.path().to_owned());
                match entry {
                    InventoryEntry::File {
                        path, size, sha256, ..
                    } => files.push((path, size, sha256)),
                    InventoryEntry::Directory { path } => directories.push(path),
                }
            }
        }
        let actual = runtime_paths(&item.path)?;
        let mut result = InstallationIntegrity {
            installation_id: id.to_owned(),
            missing: Vec::new(),
            modified: Vec::new(),
            unexpected: actual.difference(&expected).cloned().collect(),
        };
        for path in expected.difference(&actual) {
            result.missing.push(path.clone());
        }
        for path in directories {
            if actual.contains(&path)
                && crate::launch::open_nofollow(&item.path, &path, true).is_err()
            {
                result.modified.push(path);
            }
        }
        for (path, size, digest) in files {
            if !actual.contains(&path) {
                continue;
            }
            let matches = match crate::launch::open_nofollow(&item.path, &path, false) {
                Ok(file) if file.metadata()?.is_file() && file.metadata()?.len() == size => {
                    hash_open(file)? == digest
                }
                _ => false,
            };
            if !matches {
                result.modified.push(path);
            }
        }
        Ok(result)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn verify_installation(&self, _id: &str) -> Result<InstallationIntegrity, LibraryError> {
        Err(invalid(
            "installation verification is currently available only on Linux",
        ))
    }

    /// Explicit repair preserves the previous writable installation in staging. It never
    /// rewrites immutable objects or managed user data. The caller must obtain confirmation.
    #[cfg(target_os = "linux")]
    pub fn repair_installation(
        &mut self,
        id: &str,
    ) -> Result<(Installation, Option<PathBuf>), LibraryError> {
        safe_id(id)?;
        let quarantine = self
            .paths
            .content
            .join("staging")
            .join(format!("runtime-quarantine-{id}"));
        {
            let _guard = self.lifecycle_lock()?;
            let item = self.installation(id)?;
            let tracked: (Option<i64>, Option<String>) = self.conn.query_row(
                "SELECT process_id,process_start FROM installations WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if runtime_busy(&item.path, tracked.0, tracked.1.as_deref())? {
                return Err(invalid("application is running from this managed runtime"));
            }
            let old_exists = fs::symlink_metadata(&item.path).is_ok();
            let backup_exists = fs::symlink_metadata(&quarantine).is_ok();
            if old_exists && backup_exists {
                return Err(invalid(
                    "previous repair backup still exists; inspect it before repairing again",
                ));
            }
            if old_exists {
                crate::transfer::existing_dir(&item.path)?;
                rename_tree_noreplace(&item.path, &quarantine)?;
                sync_dir(&self.paths.content.join("apps"))?;
                sync_dir(&self.paths.content.join("staging"))?;
            } else if !backup_exists
                && !matches!(item.state.as_str(), "NeedsRepair" | "Preparing" | "Removed")
            {
                return Err(invalid(
                    "writable installation and repair backup are both missing",
                ));
            }
            delete_owned(
                &self.paths.content,
                &self.paths.content.join("staging"),
                &format!("runtime-{id}"),
            )?;
            self.conn.execute("UPDATE installations SET state='Removed',last_error=NULL,updated_at=?2 WHERE id=?1",params![id,now()?])?;
        }
        let item = self.installation(id)?;
        let prepared = self.prepare_portable(&item.profile_id, &item.association_id)?;
        Ok((
            prepared,
            fs::symlink_metadata(&quarantine)
                .is_ok()
                .then_some(quarantine),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn repair_installation(
        &mut self,
        _id: &str,
    ) -> Result<(Installation, Option<PathBuf>), LibraryError> {
        Err(invalid(
            "installation repair is currently available only on Linux",
        ))
    }

    /// Read-only logical deletion preview. Reflinks, compression and shared objects mean
    /// these sizes are not a promise of physical bytes returned to the filesystem.
    pub fn removal_preview(
        &self,
        id: &str,
        complete: bool,
    ) -> Result<RemovalPreview, LibraryError> {
        let installation = self.installation(id)?;
        let digest = installation
            .artifact_id
            .rsplit(':')
            .next()
            .ok_or_else(|| invalid("invalid Artifact identity"))?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("invalid Artifact digest"));
        }
        safe_id(&installation.profile_id.0)?;
        safe_id(&installation.association_id)?;
        let materialization = self
            .paths
            .content
            .join("artifacts")
            .join(&installation.profile_id.0)
            .join(&installation.association_id)
            .join(digest);
        let references: i64 = self.conn.query_row(
            "SELECT count(*) FROM verified_content WHERE artifact_id=?1 AND NOT (profile_id=?2 AND association_id=?3)",
            params![installation.artifact_id, installation.profile_id.0, installation.association_id],
            |row| row.get(0),
        )?;
        Ok(RemovalPreview {
            working_copy_logical_bytes: owned_logical_bytes(&installation.path)?,
            managed_data_logical_bytes: if complete {
                owned_logical_bytes(&installation.userdata)?
            } else {
                0
            },
            verified_materialization_logical_bytes: if complete {
                owned_logical_bytes(&materialization)?
            } else {
                0
            },
            other_verified_references: u64::try_from(references)
                .map_err(|_| invalid("invalid verified reference count"))?,
            installation,
            complete,
        })
    }

    pub fn installation_for(
        &self,
        profile: &ServerProfileId,
        association: &str,
        artifact: &str,
    ) -> Result<Option<Installation>, LibraryError> {
        let id: Option<String> = self.conn.query_row(
            "SELECT id FROM installations WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3",
            params![profile.0,association,artifact], |r| r.get(0)
        ).optional()?;
        id.map(|id| self.installation(&id)).transpose()
    }

    /// This local operation requires a completed verified download. It never downloads or
    /// interprets arbitrary declaration data. The runtime copy is independent of CAS inodes.
    #[cfg(target_os = "linux")]
    pub fn prepare_portable(
        &mut self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<Installation, LibraryError> {
        let _lock = self.lifecycle_lock()?;
        let (release, app, artifact): (String,String,String) = self.conn.query_row(
            "SELECT a.release_id,r.app_id,a.artifact_id FROM associations a JOIN releases r ON r.profile_id=a.profile_id AND r.id=a.release_id WHERE a.profile_id=?1 AND a.id=?2",
            params![profile.0,association], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))
        )?;
        if !self
            .launch_candidates(profile, &release)?
            .iter()
            .any(|candidate| {
                candidate.association_id == association
                    && matches!(candidate.status.as_str(), "PreparationRequired" | "Ready")
            })
        {
            return Err(invalid(
                "no supported NativePortableV1 launch declaration for this Association",
            ));
        }
        let completed: String = self.conn.query_row(
            "SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Completed' ORDER BY updated_at DESC LIMIT 1",
            params![profile.0,association,artifact], |r| r.get(0)
        ).optional()?.ok_or_else(|| invalid("verified download is required before preparation"))?;
        let referenced: i64 = self.conn.query_row(
            "SELECT count(*) FROM verified_content WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3",
            params![profile.0,association,artifact], |r| r.get(0)
        )?;
        if referenced != 1 {
            return Err(invalid(
                "completed download lacks one verified local reference",
            ));
        }
        let existing = self.installation_for(profile, association, &artifact)?;
        if let Some(item) = &existing {
            if item.state == "Ready" {
                crate::transfer::existing_dir(&item.path)?;
                return Ok(item.clone());
            }
            if item.state != "Removed" {
                return Err(invalid(format!(
                    "installation is {}; inspect recovery state",
                    item.state
                )));
            }
        }
        let id = existing
            .as_ref()
            .map_or_else(|| Uuid::new_v4().to_string(), |v| v.id.clone());
        safe_id(&id)?;
        let stage = self
            .paths
            .content
            .join("staging")
            .join(format!("runtime-{id}"));
        let final_path = self.paths.content.join("apps").join(&id);
        if fs::symlink_metadata(&stage).is_ok() || fs::symlink_metadata(&final_path).is_ok() {
            return Err(invalid(
                "unexpected managed runtime path; recovery required",
            ));
        }
        if existing.is_some() {
            self.conn.execute("UPDATE installations SET state='Preparing',updated_at=?2,last_error=NULL WHERE id=?1 AND state='Removed'",params![id,now()?])?;
        } else {
            self.conn.execute("INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,'Preparing',?7,?7)",
                params![id,profile.0,app,release,association,artifact,now()?])?;
        }
        let built = (|| -> Result<(), LibraryError> {
            private_dir(&stage)?;
            let root = open_dir_nofollow(&stage)?;
            if let Some(digest) = artifact.strip_prefix("adfile:sha256:") {
                let source = self.object(&artifact)?;
                let metadata = self
                    .cached_artifact(profile, &artifact)?
                    .ok_or_else(|| invalid("cached Artifact missing"))?;
                let size = metadata
                    .value
                    .size
                    .ok_or_else(|| invalid("File size missing"))?;
                let mut output = create_file_beneath(&root, "payload")?;
                copy_independent(
                    &source,
                    &mut output,
                    &stage.join("payload"),
                    size,
                    digest,
                    true,
                )?;
            } else if artifact.starts_with("adtree:v1:sha256:") {
                let entries = self
                    .tree_inventory(&completed)?
                    .ok_or_else(|| invalid("Tree inventory missing"))?;
                if identity(&entries)? != artifact {
                    return Err(invalid("Tree identity mismatch"));
                }
                validate_destination(&entries)?;
                for entry in &entries {
                    match entry {
                        InventoryEntry::Directory { path } => {
                            dir_beneath(&root, path, true)?;
                        }
                        InventoryEntry::File {
                            path,
                            size,
                            sha256,
                            executable,
                            ..
                        } => {
                            let source = self.object(&format!("adfile:sha256:{sha256}"))?;
                            let mut output = create_file_beneath(&root, path)?;
                            copy_independent(
                                &source,
                                &mut output,
                                &stage.join(path),
                                *size,
                                sha256,
                                *executable,
                            )?;
                        }
                    }
                }
                for entry in entries.iter().rev() {
                    if let InventoryEntry::Directory { path } = entry {
                        dir_beneath(&root, path, false)?.sync_all()?;
                    }
                }
            } else {
                return Err(invalid("unsupported Artifact identity"));
            }
            root.sync_all()?;
            rename_tree_noreplace(&stage, &final_path)?;
            sync_dir(&self.paths.content.join("apps"))?;
            private_dir(&self.paths.content.join("userdata").join(&id))?;
            self.conn.execute("UPDATE installations SET state='Ready',updated_at=?2 WHERE id=?1 AND state='Preparing'",params![id,now()?])?;
            Ok(())
        })();
        if let Err(error) = built {
            self.conn.execute("UPDATE installations SET state='NeedsRepair',last_error=?2,updated_at=?3 WHERE id=?1 AND state='Preparing'",params![id,error.to_string(),now()?])?;
            return Err(error);
        }
        self.installation(&id)
    }

    /// Explicitly rebuild a damaged Tree materialization from verified immutable file objects.
    /// The previous directory is preserved in staging as a quarantine, including extra files.
    #[cfg(target_os = "linux")]
    pub fn repair_verified_tree(
        &mut self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<(PathBuf, Option<PathBuf>), LibraryError> {
        let _lock = self.lifecycle_lock()?;
        let job_id:String=self.conn.query_row("SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id LIKE 'adtree:v1:sha256:%' AND state='Completed' ORDER BY updated_at DESC LIMIT 1",params![profile.0,association],|r|r.get(0))?;
        safe_id(&job_id)?;
        let job = self.job(&job_id)?;
        let entries = self
            .tree_inventory(&job_id)?
            .ok_or_else(|| invalid("verified Tree inventory is unavailable"))?;
        if identity(&entries)? != job.artifact_id {
            return Err(invalid("Tree inventory identity mismatch"));
        }
        validate_destination(&entries)?;
        let final_path = crate::tree_download::tree_destination(self, &job)?;
        let stage = self
            .paths
            .content
            .join("staging")
            .join(format!("source-repair-{job_id}"));
        let quarantine = self
            .paths
            .content
            .join("staging")
            .join(format!("source-quarantine-{job_id}"));
        if fs::symlink_metadata(&quarantine).is_ok() && fs::symlink_metadata(&final_path).is_ok() {
            let (valid, _) = self.inspect_trees(Some(profile))?;
            if valid.iter().any(|item| {
                item.association_id == association && item.artifact_id == job.artifact_id
            }) {
                return Ok((final_path, Some(quarantine)));
            }
            return Err(invalid(
                "previous Tree quarantine exists and current source is damaged; inspect both before retrying",
            ));
        }
        if fs::symlink_metadata(&stage).is_ok() {
            delete_owned(
                &self.paths.content,
                &self.paths.content.join("staging"),
                &format!("source-repair-{job_id}"),
            )?;
        }
        private_dir(&stage)?;
        let root = open_dir_nofollow(&stage)?;
        for entry in &entries {
            match entry {
                InventoryEntry::Directory { path } => {
                    dir_beneath(&root, path, true)?;
                }
                InventoryEntry::File {
                    path,
                    size,
                    sha256,
                    executable,
                    ..
                } => {
                    let source = self.object(&format!("adfile:sha256:{sha256}"))?;
                    let mut output = create_file_beneath(&root, path)?;
                    copy_independent(
                        &source,
                        &mut output,
                        &stage.join(path),
                        *size,
                        sha256,
                        *executable,
                    )?;
                    use std::os::unix::fs::PermissionsExt;
                    output.set_permissions(fs::Permissions::from_mode(if *executable {
                        0o500
                    } else {
                        0o400
                    }))?;
                    output.sync_all()?;
                }
            }
        }
        for entry in entries.iter().rev() {
            if let InventoryEntry::Directory { path } = entry {
                dir_beneath(&root, path, false)?.sync_all()?;
            }
        }
        root.sync_all()?;
        let mut old = None;
        if fs::symlink_metadata(&final_path).is_ok() {
            if fs::symlink_metadata(&quarantine).is_ok() {
                return Err(invalid(
                    "previous Tree quarantine already exists; inspect it before repairing again",
                ));
            }
            let association_dir = final_path
                .parent()
                .ok_or_else(|| invalid("Tree destination parent missing"))?;
            let parent = crate::launch::open_nofollow(association_dir, "", true)?;
            let name = final_path
                .file_name()
                .ok_or_else(|| invalid("Tree destination name missing"))?;
            validate_owned_mounts(&parent, name, statx_handle(&parent)?.stx_mnt_id)?;
            rename_tree_noreplace(&final_path, &quarantine)?;
            old = Some(quarantine.clone());
            sync_dir(&self.paths.content.join("staging"))?;
        } else if fs::symlink_metadata(&quarantine).is_ok() {
            old = Some(quarantine.clone());
        }
        rename_tree_noreplace(&stage, &final_path)?;
        sync_dir(
            final_path
                .parent()
                .ok_or_else(|| invalid("Tree destination parent missing"))?,
        )?;
        let (valid, issues) = self.inspect_trees(Some(profile))?;
        if !valid
            .iter()
            .any(|v| v.association_id == association && v.artifact_id == job.artifact_id)
        {
            return Err(invalid(format!(
                "repaired Tree could not be verified: {}",
                issues
                    .first()
                    .map_or("unknown error", |v| v.reason.as_str())
            )));
        }
        Ok((final_path, old))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn repair_verified_tree(
        &mut self,
        _profile: &ServerProfileId,
        _association: &str,
    ) -> Result<(PathBuf, Option<PathBuf>), LibraryError> {
        Err(invalid("Tree repair is currently available only on Linux"))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn prepare_portable(
        &mut self,
        _profile: &ServerProfileId,
        _association: &str,
    ) -> Result<Installation, LibraryError> {
        Err(invalid(
            "portable runtime preparation is currently available only on Linux",
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn runtime_copy_is_writable_and_never_shares_source_inode() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"original").unwrap();
        let destination_path = temp.path().join("runtime");
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination_path)
            .unwrap();
        copy_independent(
            &source,
            &mut destination,
            &destination_path,
            8,
            &format!("{:x}", Sha256::digest(b"original")),
            true,
        )
        .unwrap();
        assert_ne!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&destination_path).unwrap().ino()
        );
        fs::write(&destination_path, b"modified").unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
    }

    #[test]
    fn portable_file_prepares_and_launches_from_independent_runtime() {
        use crate::library::LibraryPaths;
        let temp = tempfile::tempdir().unwrap();
        let mut lib = Library::open(LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        })
        .unwrap();
        let bytes = fs::read("/bin/true").unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let artifact = format!("adfile:sha256:{digest}");
        let profile = ServerProfileId(Uuid::new_v4().to_string());
        let association = Uuid::new_v4().to_string();
        let job = Uuid::new_v4().to_string();
        let snapshot = serde_json::json!({"releaseId":"release","app":{"revisionId":"a","data":{}},"release":{"revisionId":"r","data":{}},"associations":[{"associationId":association,"declaration":{"revisionId":"rev","data":{"appDepot":{"entryPoints":[{"id":"run","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1"}}]}}}}]});
        lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[&profile.0]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",
                params![
                    profile.0,
                    r#"{"id":"app","name":"File","visibility":"Public","state":"Published"}"#
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile.0,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile.0,
                    artifact,
                    serde_json::json!({"id":artifact,"kind":"File","size":bytes.len()}).to_string()
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile.0,association,artifact,serde_json::json!({"id":association,"releaseId":"release","artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO snapshots(profile_id,release_id,snapshot_json,app_revision,release_revision,last_seen) VALUES (?1,'release',?2,'a','r',0)",params![profile.0,snapshot.to_string()]).unwrap();
        lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at) VALUES (?1,?2,?3,'Completed',0,?4,?5,0)",params![job,profile.0,artifact,association,bytes.len() as i64]).unwrap();
        let materialization = lib
            .paths
            .content
            .join("artifacts")
            .join(&profile.0)
            .join(&association)
            .join(&digest);
        fs::create_dir_all(materialization.parent().unwrap()).unwrap();
        fs::write(&materialization, &bytes).unwrap();
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,0,?4)",params![profile.0,artifact,format!("artifacts/{}/{}/{}",profile.0,association,digest),association]).unwrap();
        let object = lib.object(&artifact).unwrap();
        fs::create_dir_all(object.parent().unwrap()).unwrap();
        fs::write(&object, &bytes).unwrap();
        let installation = lib.prepare_portable(&profile, &association).unwrap();
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "Ready"
        );
        assert_ne!(
            fs::metadata(&object).unwrap().ino(),
            fs::metadata(installation.path.join("payload"))
                .unwrap()
                .ino()
        );
        assert!(
            lib.play_declared(&profile, "release", &association, "run", "rev")
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
        fs::write(installation.userdata.join("owned-save"), b"remove").unwrap();
        let external = temp.path().join("external-save");
        fs::write(&external, b"keep").unwrap();
        std::os::unix::fs::symlink(&external, installation.path.join("external-save")).unwrap();
        let preview = lib
            .local_uninstall_preview(&profile, "app")
            .unwrap()
            .unwrap();
        assert_eq!(preview.states, ["Ready"]);
        assert!(preview.runtime_bytes > bytes.len() as u64);
        assert!(lib.uninstall_local_app(&profile, "app", "stale").is_err());
        // A parallel test's fork can inherit a descriptor until its exec.
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let slot = loop {
            match lib.lock_download_slot() {
                Ok(slot) => break slot,
                Err(LibraryError::Invalid(message))
                    if message == "another application download holds the library slot"
                        && std::time::Instant::now() < limit =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        assert!(
            lib.local_uninstall_preview(&profile, "app")
                .unwrap()
                .unwrap()
                .blocked
        );
        assert!(
            lib.uninstall_local_app(&profile, "app", &preview.revision)
                .is_err()
        );
        slot.unlock().unwrap();
        drop(slot);
        assert!(
            lib.uninstall_local_app(&profile, "app", &preview.revision)
                .unwrap()
        );
        assert!(!installation.path.exists());
        assert!(!installation.userdata.exists());
        assert_eq!(fs::read(external).unwrap(), b"keep");
        assert!(
            lib.local_uninstall_preview(&profile, "app")
                .unwrap()
                .is_none()
        );
        assert!(
            lib.uninstall_local_app(&profile, "app", &preview.revision)
                .is_err()
        );
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "NotDownloaded"
        );
    }

    #[test]
    fn managed_delete_unlinks_symlink_without_following_target() {
        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned");
        fs::create_dir(&owned).unwrap();
        fs::write(temp.path().join("outside"), b"safe").unwrap();
        let target = owned.join("one");
        fs::create_dir(&target).unwrap();
        symlink(temp.path().join("outside"), target.join("link")).unwrap();
        fs::write(target.join("created.log"), b"runtime data").unwrap();
        delete_owned(temp.path(), &owned, "one").unwrap();
        assert_eq!(fs::read(temp.path().join("outside")).unwrap(), b"safe");
        assert!(!target.exists());
        assert!(delete_owned(temp.path(), &owned, "../outside").is_err());
    }

    #[test]
    fn portable_tree_launch_writes_only_runtime_and_reprepares_offline() {
        use crate::{library::LibraryPaths, models::InventoryEntry};
        use rusqlite::params;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("game.rs");
        fs::write(&source,r#"fn main(){std::fs::write("23.log",b"runtime log").unwrap();std::fs::write("config.ini",b"modified").unwrap();std::thread::sleep(std::time::Duration::from_millis(250));}"#).unwrap();
        let executable = temp.path().join("game");
        assert!(
            std::process::Command::new("rustc")
                .args([
                    "--edition=2024",
                    source.to_str().unwrap(),
                    "-o",
                    executable.to_str().unwrap()
                ])
                .status()
                .unwrap()
                .success()
        );
        let binary = fs::read(&executable).unwrap();
        let config = b"original";
        let binary_hash = format!("{:x}", Sha256::digest(&binary));
        let config_hash = format!("{:x}", Sha256::digest(config));
        let binary_id = Uuid::new_v4().to_string();
        let config_id = Uuid::new_v4().to_string();
        let entries = vec![
            InventoryEntry::Directory {
                path: "System".into(),
            },
            InventoryEntry::File {
                path: "System/config.ini".into(),
                file_id: config_id,
                size: config.len() as u64,
                sha256: config_hash.clone(),
                executable: false,
            },
            InventoryEntry::File {
                path: "System/game".into(),
                file_id: binary_id,
                size: binary.len() as u64,
                sha256: binary_hash.clone(),
                executable: true,
            },
        ];
        let tree_id = identity(&entries).unwrap();
        let mut lib = Library::open(LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        })
        .unwrap();
        let profile = ServerProfileId(Uuid::new_v4().to_string());
        let association = Uuid::new_v4().to_string();
        let job = Uuid::new_v4().to_string();
        lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[&profile.0]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",
                params![
                    profile.0,
                    r#"{"id":"app","name":"Test","visibility":"Public","state":"Published"}"#
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile.0,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
        lib.conn.execute("INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",params![profile.0,tree_id,serde_json::json!({"id":tree_id,"kind":"Tree","size":binary.len()+config.len()}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile.0,association,tree_id,serde_json::json!({"id":association,"releaseId":"release","artifactId":tree_id}).to_string()]).unwrap();
        let snapshot = serde_json::json!({"releaseId":"release","app":{"revisionId":"a","data":{}},"release":{"revisionId":"r","data":{}},"associations":[{"associationId":association,"declaration":{"revisionId":"rev","data":{"appDepot":{"entryPoints":[{"id":"game","target":{"type":"TreeFile","path":"System/game"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"System"}}]}}}}]});
        lib.conn.execute("INSERT INTO snapshots(profile_id,release_id,snapshot_json,app_revision,release_revision,last_seen) VALUES (?1,'release',?2,'a','r',0)",params![profile.0,snapshot.to_string()]).unwrap();
        lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at,inventory_json) VALUES (?1,?2,?3,'Completed',0,?4,?5,0,?6)",params![job,profile.0,tree_id,association,(binary.len()+config.len()) as i64,serde_json::to_string(&entries).unwrap()]).unwrap();
        for entry in &entries {
            if let InventoryEntry::File {
                path,
                file_id,
                size,
                sha256,
                executable,
            } = entry
            {
                lib.conn.execute("INSERT INTO tree_files(job_id,path,file_id,size,sha256,executable,progress,verified) VALUES (?1,?2,?3,?4,?5,?6,?4,1)",params![job,path,file_id,*size as i64,sha256,i64::from(*executable)]).unwrap();
            }
        }
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,'tree',0,?3)",params![profile.0,tree_id,association]).unwrap();
        for (digest, bytes) in [
            (&binary_hash, binary.as_slice()),
            (&config_hash, config.as_slice()),
        ] {
            let object = lib.object(&format!("adfile:sha256:{digest}")).unwrap();
            fs::create_dir_all(object.parent().unwrap()).unwrap();
            fs::write(object, bytes).unwrap();
        }
        let original = lib
            .paths
            .content
            .join("artifacts")
            .join(&profile.0)
            .join(&association)
            .join(tree_id.rsplit(':').next().unwrap());
        fs::create_dir_all(original.join("System")).unwrap();
        fs::write(original.join("System/game"), &binary).unwrap();
        fs::write(original.join("System/config.ini"), config).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            original.join("System/game"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "PreparationRequired"
        );
        let install = lib.prepare_portable(&profile, &association).unwrap();
        let preview = lib.removal_preview(&install.id, true).unwrap();
        assert_eq!(
            preview.working_copy_logical_bytes,
            (binary.len() + config.len()) as u64
        );
        assert_eq!(
            preview.verified_materialization_logical_bytes,
            (binary.len() + config.len()) as u64
        );
        assert_eq!(preview.other_verified_references, 0);
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "Ready"
        );
        let mut child = lib
            .play_declared(&profile, "release", &association, "game", "rev")
            .unwrap();
        drop(lib);
        let mut lib = Library::open(LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        })
        .unwrap();
        assert!(lib.remove_installation(&install.id, false).is_err());
        assert!(child.wait().unwrap().success());
        assert_eq!(
            fs::read(install.path.join("System/23.log")).unwrap(),
            b"runtime log"
        );
        assert_eq!(
            fs::read(install.path.join("System/config.ini")).unwrap(),
            b"modified"
        );
        assert_eq!(
            fs::read(original.join("System/config.ini")).unwrap(),
            b"original"
        );
        let changed = lib.verify_installation(&install.id).unwrap();
        assert!(changed.modified.contains(&"System/config.ini".to_owned()));
        assert!(changed.unexpected.contains(&"System/23.log".to_owned()));
        fs::copy("/bin/true", install.path.join("System/game")).unwrap();
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "Ready"
        );
        assert!(
            lib.play_declared(&profile, "release", &association, "game", "rev")
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
        let changed = lib.verify_installation(&install.id).unwrap();
        assert!(changed.modified.contains(&"System/game".to_owned()));
        let (_, runtime_backup) = lib.repair_installation(&install.id).unwrap();
        assert_eq!(
            fs::read(runtime_backup.unwrap().join("System/23.log")).unwrap(),
            b"runtime log"
        );
        assert_eq!(
            fs::read(
                lib.paths
                    .content
                    .join("staging")
                    .join(format!("runtime-quarantine-{}", install.id))
                    .join("System/config.ini")
            )
            .unwrap(),
            b"modified"
        );
        assert!(
            lib.verify_installation(&install.id)
                .unwrap()
                .matches_original()
        );
        assert!(
            lib.play_declared(&profile, "release", &association, "game", "rev")
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
        assert!(lib.verified_trees(Some(&profile)).is_ok());
        assert_eq!(lib.collect_unreferenced_objects().unwrap(), 0);
        lib.conn
            .execute(
                "DELETE FROM tree_files WHERE job_id=?1 AND path='System/config.ini'",
                [&job],
            )
            .unwrap();
        assert!(lib.collect_unreferenced_objects().is_err());
        lib.conn.execute("INSERT INTO tree_files(job_id,path,file_id,size,sha256,executable,progress,verified) VALUES (?1,'System/config.ini',?2,8,?3,0,8,1)",params![job,entries.iter().find_map(|e|if let InventoryEntry::File{path,file_id,..}=e && path=="System/config.ini"{Some(file_id)}else{None}).unwrap(),config_hash]).unwrap();
        let other_association = Uuid::new_v4().to_string();
        let other_hash = format!("{:x}", Sha256::digest(b"unrelated"));
        let other_id = format!("adfile:sha256:{other_hash}");
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile.0,
                    other_id,
                    serde_json::json!({"id":other_id,"kind":"File","size":9}).to_string()
                ],
            )
            .unwrap();
        let other_path = lib
            .paths
            .content
            .join("artifacts")
            .join(&profile.0)
            .join(&other_association)
            .join(&other_hash);
        fs::create_dir_all(other_path.parent().unwrap()).unwrap();
        fs::write(&other_path, b"unrelated").unwrap();
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,0,?4)",params![profile.0,other_id,format!("artifacts/{}/{}/{}",profile.0,other_association,other_hash),other_association]).unwrap();
        fs::write(original.join("System/stray.log"), b"legacy runtime file").unwrap();
        let (valid, issues) = lib.inspect_trees(Some(&profile)).unwrap();
        assert!(valid.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(lib.inspect_files(Some(&profile)).unwrap().0.len(), 1);
        assert_eq!(
            lib.launch_candidates(&profile, "release").unwrap()[0].status,
            "Ready"
        );
        let (_, quarantine) = lib.repair_verified_tree(&profile, &association).unwrap();
        assert!(lib.repair_verified_tree(&profile, &association).is_ok());
        assert_eq!(
            fs::read(quarantine.unwrap().join("System/stray.log")).unwrap(),
            b"legacy runtime file"
        );
        assert!(lib.verified_trees(Some(&profile)).is_ok());
        fs::write(install.userdata.join("save.dat"), b"save").unwrap();
        lib.remove_installation(&install.id, false).unwrap();
        assert!(!install.path.exists());
        assert_eq!(
            fs::read(install.userdata.join("save.dat")).unwrap(),
            b"save"
        );
        let repaired = lib.prepare_portable(&profile, &association).unwrap();
        assert_eq!(repaired.id, install.id);
        assert!(!repaired.path.join("System/23.log").exists());
        lib.remove_installation(&repaired.id, true).unwrap();
        assert!(!repaired.userdata.exists());
        assert!(!original.exists());
        assert_eq!(
            lib.conn
                .query_row("SELECT count(*) FROM verified_content", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn data_only_download_removal_keeps_shared_object_until_last_profile() {
        use crate::library::LibraryPaths;
        use rusqlite::params;
        let temp = tempfile::tempdir().unwrap();
        let mut lib = Library::open(LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        })
        .unwrap();
        let digest = format!("{:x}", Sha256::digest(b"shared bytes"));
        let artifact = format!("adfile:sha256:{digest}");
        let object = lib.object(&artifact).unwrap();
        fs::create_dir_all(object.parent().unwrap()).unwrap();
        fs::write(&object, b"shared bytes").unwrap();
        let mut profiles = Vec::new();
        for _ in 0..2 {
            let profile = ServerProfileId(Uuid::new_v4().to_string());
            let association = Uuid::new_v4().to_string();
            let job = Uuid::new_v4().to_string();
            lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[&profile.0]).unwrap();
            lib.conn.execute("INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",params![profile.0,r#"{"id":"app","name":"Data","visibility":"Public","state":"Published"}"#]).unwrap();
            lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile.0,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
            lib.conn.execute("INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",params![profile.0,artifact,serde_json::json!({"id":artifact,"kind":"File","size":12}).to_string()]).unwrap();
            lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile.0,association,artifact,serde_json::json!({"id":association,"releaseId":"release","artifactId":artifact}).to_string()]).unwrap();
            lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at) VALUES (?1,?2,?3,'Completed',0,?4,12,0)",params![job,profile.0,artifact,association]).unwrap();
            lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,'file',0,?3)",params![profile.0,artifact,association]).unwrap();
            let material = lib
                .paths
                .content
                .join("artifacts")
                .join(&profile.0)
                .join(&association)
                .join(&digest);
            fs::create_dir_all(material.parent().unwrap()).unwrap();
            fs::hard_link(&object, &material).unwrap();
            profiles.push((profile, association, material));
        }
        let (first, association, path) = &profiles[0];
        let extra = Uuid::new_v4().to_string();
        let extra_job = Uuid::new_v4().to_string();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release-2','app',?2,0)",params![first.0,r#"{"id":"release-2","appId":"app","state":"Published"}"#]).unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release-2',?3,?4,0)",params![first.0,extra,artifact,serde_json::json!({"id":extra,"releaseId":"release-2","artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at) VALUES (?1,?2,?3,'Completed',0,?4,12,0)",params![extra_job,first.0,artifact,extra]).unwrap();
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,'extra-file',0,?3)",params![first.0,artifact,extra]).unwrap();
        lib.conn
            .execute(
                "UPDATE download_jobs SET state='Queued' WHERE profile_id=?1 AND association_id=?2",
                params![first.0, association],
            )
            .unwrap();
        let busy = lib.local_uninstall_preview(first, "app").unwrap().unwrap();
        assert!(busy.blocked);
        assert!(
            lib.uninstall_local_app(first, "app", &busy.revision)
                .is_err()
        );
        assert!(path.exists());
        lib.conn.execute("UPDATE download_jobs SET state='Completed' WHERE profile_id=?1 AND association_id=?2", params![first.0,association]).unwrap();
        let preview = lib.local_uninstall_preview(first, "app").unwrap().unwrap();
        assert_eq!(preview.download_bytes, 24);
        // Reopen after the durable batch commit, before any filesystem work.
        {
            let _lifecycle = lib.lifecycle_lock().unwrap();
            let _download = lib.lock_download_slot().unwrap();
            assert_eq!(
                lib.journal_uninstall(first, "app", &preview.revision)
                    .unwrap()
                    .len(),
                2
            );
            assert!(
                lib.conn
                    .execute(
                        "UPDATE download_jobs SET state='Queued' WHERE id=?1",
                        [&extra_job]
                    )
                    .is_err()
            );
        }
        assert!(path.exists());
        let paths = lib.paths.clone();
        drop(lib);
        let mut lib = Library::open(paths).unwrap();
        assert!(!path.exists());
        assert!(lib.local_uninstall_preview(first, "app").unwrap().is_none());
        assert!(object.exists());
        let (second, association, path) = &profiles[1];
        assert!(path.exists());
        lib.remove_local_artifact(second, association).unwrap();
        assert!(!path.exists());
        assert!(!object.exists());
        assert_eq!(
            lib.conn
                .query_row("SELECT count(*) FROM profiles", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn interrupted_removal_is_reconciled_after_reopen() {
        use crate::library::LibraryPaths;
        use rusqlite::params;
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        };
        let lib = Library::open(paths.clone()).unwrap();
        let profile = ServerProfileId(Uuid::new_v4().to_string());
        let association = Uuid::new_v4().to_string();
        let id = Uuid::new_v4().to_string();
        let artifact = format!("adfile:sha256:{}", "0".repeat(64));
        lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[&profile.0]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",
                params![
                    profile.0,
                    r#"{"id":"app","name":"Test","visibility":"Public","state":"Published"}"#
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile.0,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile.0,
                    artifact,
                    serde_json::json!({"id":artifact,"kind":"File","size":0}).to_string()
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile.0,association,artifact,serde_json::json!({"id":association,"releaseId":"release","artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at,remove_complete) VALUES (?1,?2,'app','release',?3,?4,'Removing',0,0,0)",params![id,profile.0,association,artifact]).unwrap();
        let runtime = lib.paths.content.join("apps").join(&id);
        fs::create_dir(&runtime).unwrap();
        fs::write(runtime.join("log"), b"runtime").unwrap();
        let userdata = lib.paths.content.join("userdata").join(&id);
        fs::create_dir(&userdata).unwrap();
        fs::write(userdata.join("save"), b"keep").unwrap();
        drop(lib);
        let recovered = Library::open(paths).unwrap();
        assert_eq!(recovered.installation(&id).unwrap().state, "Removed");
        assert!(!runtime.exists());
        assert_eq!(fs::read(userdata.join("save")).unwrap(), b"keep");
    }

    #[test]
    fn removal_child() {
        let Some(root) = std::env::var_os("APP_DEPOT_REMOVE_CHILD") else {
            return;
        };
        let root = PathBuf::from(root);
        let mut lib = Library::open(crate::library::LibraryPaths {
            metadata: root.join("meta"),
            content: root.join("content"),
        })
        .unwrap();
        let id = std::env::var("APP_DEPOT_REMOVE_ID").unwrap();
        lib.remove_installation(&id, false).unwrap();
    }

    #[test]
    fn sigkill_during_removal_recovers_without_deleting_userdata() {
        use crate::library::LibraryPaths;
        use rusqlite::params;
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        };
        let lib = Library::open(paths.clone()).unwrap();
        let profile = ServerProfileId(Uuid::new_v4().to_string());
        let association = Uuid::new_v4().to_string();
        let id = Uuid::new_v4().to_string();
        let artifact = format!("adfile:sha256:{}", "0".repeat(64));
        lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[&profile.0]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",
                params![
                    profile.0,
                    r#"{"id":"app","name":"Test","visibility":"Public","state":"Published"}"#
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile.0,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile.0,
                    artifact,
                    serde_json::json!({"id":artifact,"kind":"File","size":0}).to_string()
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile.0,association,artifact,serde_json::json!({"id":association,"releaseId":"release","artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at) VALUES (?1,?2,'app','release',?3,?4,'Ready',0,0)",params![id,profile.0,association,artifact]).unwrap();
        let runtime = lib.paths.content.join("apps").join(&id);
        fs::create_dir(&runtime).unwrap();
        for n in 0..2_000 {
            fs::write(runtime.join(format!("file-{n:04}")), b"runtime").unwrap();
        }
        let userdata = lib.paths.content.join("userdata").join(&id);
        fs::create_dir(&userdata).unwrap();
        fs::write(userdata.join("save"), b"keep").unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "managed::tests::removal_child"])
            .env("APP_DEPOT_REMOVE_CHILD", temp.path())
            .env("APP_DEPOT_REMOVE_ID", &id)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let mut observed = false;
        while started.elapsed() < std::time::Duration::from_secs(5) {
            let state: String = lib
                .conn
                .query_row("SELECT state FROM installations WHERE id=?1", [&id], |r| {
                    r.get(0)
                })
                .unwrap();
            if state == "Removing" && runtime.exists() {
                observed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(observed, "child did not expose an in-progress removal");
        unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        assert!(!child.wait().unwrap().success());
        drop(lib);
        let recovered = Library::open(paths).unwrap();
        assert_eq!(recovered.installation(&id).unwrap().state, "Removed");
        assert!(!runtime.exists());
        assert_eq!(fs::read(userdata.join("save")).unwrap(), b"keep");
    }
}
