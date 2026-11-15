//! Durable Tree Artifact transfer and materialization. Tree members are content objects, never
//! standalone File Artifact associations.
use crate::{
    ServerProfileId,
    api::{ApiClient, ApiError},
    library::{Library, LibraryError, now},
    models::InventoryEntry,
    transfer::{
        BlobOutcome, BlobSpec, DownloadJob, existing_dir, hash_file, open_regular, private_dir,
        receive_blob, safe_regular, sync_dir,
    },
    tree::{identity, validate_destination},
};
use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use uuid::Uuid;

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeFileProgress {
    pub path: String,
    pub file_id: String,
    pub size: u64,
    pub progress: u64,
    pub verified: bool,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedTree {
    pub profile_id: ServerProfileId,
    pub association_id: String,
    pub artifact_id: String,
    pub path: PathBuf,
    pub file_count: usize,
    pub verified_at: i64,
}

fn stage_root(library: &Library, id: &str) -> Result<PathBuf, LibraryError> {
    Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
    Ok(library.paths.content.join("staging").join(id))
}
fn child_stage(library: &Library, id: &str, file_id: &str) -> Result<PathBuf, LibraryError> {
    Uuid::parse_str(file_id).map_err(|_| invalid("invalid tree file ID"))?;
    Ok(stage_root(library, id)?
        .join("files")
        .join(format!("{file_id}.part")))
}
fn tree_destination(library: &Library, job: &DownloadJob) -> Result<PathBuf, LibraryError> {
    Uuid::parse_str(&job.profile_id.0).map_err(|_| invalid("invalid profile ID"))?;
    Uuid::parse_str(&job.association_id).map_err(|_| invalid("invalid association ID"))?;
    let digest = job
        .artifact_id
        .strip_prefix("adtree:v1:sha256:")
        .ok_or_else(|| invalid("invalid Tree Artifact ID"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid Tree Artifact digest"));
    }
    Ok(library
        .paths
        .content
        .join("artifacts")
        .join(&job.profile_id.0)
        .join(&job.association_id)
        .join(digest))
}

#[cfg(unix)]
fn open_dir_nofollow(path: &Path) -> Result<File, LibraryError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(invalid("unsafe Tree directory"));
    }
    Ok(file)
}
#[cfg(unix)]
fn child_dir(parent: &File, name: &str, create: bool) -> Result<File, LibraryError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).map_err(|_| invalid("NUL in Tree path"))?;
    if create {
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
    }
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
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_dir() {
        return Err(invalid("unsafe Tree directory"));
    }
    Ok(file)
}
#[cfg(unix)]
fn dir_beneath(root: &File, path: &str, create: bool) -> Result<File, LibraryError> {
    let mut current = root.try_clone()?;
    for component in path.split('/') {
        current = child_dir(&current, component, create)?;
    }
    Ok(current)
}
#[cfg(unix)]
fn create_file_beneath(root: &File, path: &str) -> Result<File, LibraryError> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let directory = if parent.is_empty() {
        root.try_clone()?
    } else {
        dir_beneath(root, parent, false)?
    };
    let name = CString::new(name).map_err(|_| invalid("NUL in Tree path"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(invalid("unsafe Tree file"));
    }
    Ok(file)
}
#[cfg(unix)]
fn unlink_beneath(root: &File, path: &str, directory: bool) -> Result<(), LibraryError> {
    use std::{ffi::CString, os::fd::AsRawFd};
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let handle = if parent.is_empty() {
        root.try_clone()?
    } else {
        match dir_beneath(root, parent, false) {
            Ok(value) => value,
            Err(LibraryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    };
    let name = CString::new(name).map_err(|_| invalid("NUL in Tree path"))?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    let result = unsafe { libc::unlinkat(handle.as_raw_fd(), name.as_ptr(), flags) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

fn rename_tree_noreplace(from: &Path, to: &Path) -> Result<(), LibraryError> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid staging path"))?;
        let to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid destination path"))?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (from, to);
        Err(invalid(
            "atomic no-replace Tree promotion is not yet supported on this platform",
        ))
    }
}

impl Library {
    pub fn queue_tree(
        &mut self,
        client: &ApiClient,
        profile_id: &ServerProfileId,
        release_id: &str,
        association_id: &str,
    ) -> Result<DownloadJob, LibraryError> {
        let profile = self.profile(profile_id)?;
        if profile.archived || profile.origin != client.origin() {
            return Err(invalid("active profile and client origin must match"));
        }
        Uuid::parse_str(association_id).map_err(|_| invalid("invalid Association ID"))?;
        let caps = client.capabilities()?;
        if caps.api_version != "1.0.0"
            || caps.tree_identity != "adtree:v1:sha256"
            || caps.file_identity != "adfile:sha256"
            || !caps.download_ranges
        {
            return Err(LibraryError::Api(ApiError::Incompatible(
                "Tree transfer capabilities are unsupported".into(),
            )));
        }
        let release = client.release(release_id)?;
        let app = client.app(&release.app_id)?;
        let association = client
            .list_associations(
                release_id,
                caps.page_size().map_err(ApiError::Incompatible)?,
            )?
            .into_iter()
            .find(|value| value.id == association_id)
            .ok_or_else(|| invalid("Association is absent from accessible Release"))?;
        let artifact = client.artifact(&association.artifact_id)?;
        if artifact.kind != "Tree" {
            return Err(invalid("Association is not a Tree Artifact"));
        }
        let id = Uuid::new_v4().to_string();
        let time = now()?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,?4) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json,last_seen=excluded.last_seen",params![profile_id.0,app.id,serde_json::to_string(&app)?,time])?;
        tx.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(profile_id,id) DO UPDATE SET app_id=excluded.app_id,metadata_json=excluded.metadata_json,last_seen=excluded.last_seen",params![profile_id.0,release.id,release.app_id,serde_json::to_string(&release)?,time])?;
        tx.execute("INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,?4) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json,last_seen=excluded.last_seen",params![profile_id.0,artifact.id,serde_json::to_string(&artifact)?,time])?;
        tx.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(profile_id,id) DO UPDATE SET release_id=excluded.release_id,artifact_id=excluded.artifact_id,metadata_json=excluded.metadata_json,last_seen=excluded.last_seen",params![profile_id.0,association.id,association.release_id,association.artifact_id,serde_json::to_string(&association)?,time])?;
        tx.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,association_id,state,created_at,updated_at,expected_size) VALUES (?1,?2,?3,?4,'Queued',?5,?5,0)",params![id,profile_id.0,artifact.id,association.id,time])?;
        tx.commit()?;
        self.job(&id)
    }

    fn tree_inventory(&self, id: &str) -> Result<Option<Vec<InventoryEntry>>, LibraryError> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT inventory_json FROM download_jobs WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        raw.map(|value| serde_json::from_str(&value).map_err(LibraryError::from))
            .transpose()
    }

    fn fetch_tree_inventory(
        &mut self,
        client: &ApiClient,
        job: &DownloadJob,
    ) -> Result<Vec<InventoryEntry>, LibraryError> {
        if let Some(entries) = self.tree_inventory(&job.id)? {
            if identity(&entries)? != job.artifact_id {
                return Err(invalid("persisted Tree inventory identity mismatch"));
            }
            validate_destination(&entries)?;
            return Ok(entries);
        }
        self.state(&job.id, "FetchingInventory", None)?;
        let caps = client.capabilities()?;
        if caps.api_version != "1.0.0" || caps.tree_identity != "adtree:v1:sha256" {
            return Err(LibraryError::Api(ApiError::Incompatible(
                "unsupported Tree capability".into(),
            )));
        }
        let (entries, etag) = client.tree_inventory(
            &job.artifact_id,
            caps.page_size().map_err(ApiError::Incompatible)?,
        )?;
        if identity(&entries)? != job.artifact_id {
            return Err(invalid("Tree identity mismatch"));
        }
        validate_destination(&entries)?;
        let mut total = 0u64;
        for entry in &entries {
            if let InventoryEntry::File { size, .. } = entry {
                total = total
                    .checked_add(*size)
                    .ok_or_else(|| invalid("Tree byte total overflow"))?;
            }
        }
        if total > i64::MAX as u64 {
            return Err(invalid("Tree exceeds supported local size"));
        }
        let tx = self.conn.transaction()?;
        for entry in &entries {
            if let InventoryEntry::File {
                path,
                file_id,
                size,
                sha256,
                executable,
            } = entry
            {
                if *size > i64::MAX as u64 {
                    return Err(invalid("Tree member exceeds supported local size"));
                }
                tx.execute("INSERT INTO tree_files(job_id,path,file_id,size,sha256,executable) VALUES (?1,?2,?3,?4,?5,?6)",params![job.id,path,file_id,*size as i64,sha256,i64::from(*executable)])?;
            }
        }
        tx.execute("UPDATE download_jobs SET inventory_json=?2,inventory_etag=?3,expected_size=?4,state='Downloading',updated_at=?5 WHERE id=?1 AND state='FetchingInventory'",params![job.id,serde_json::to_string(&entries)?,etag,total as i64,now()?])?;
        tx.commit()?;
        Ok(entries)
    }

    pub fn tree_files(&self, id: &str) -> Result<Vec<TreeFileProgress>, LibraryError> {
        let mut stmt = self.conn.prepare("SELECT path,file_id,size,progress,verified,etag FROM tree_files WHERE job_id=?1 ORDER BY path")?;
        Ok(stmt
            .query_map([id], |r| {
                let size: i64 = r.get(2)?;
                let progress: i64 = r.get(3)?;
                let verified: i64 = r.get(4)?;
                Ok(TreeFileProgress {
                    path: r.get(0)?,
                    file_id: r.get(1)?,
                    size: size as u64,
                    progress: progress as u64,
                    verified: verified != 0,
                    etag: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn tree_checkpoint(
        &mut self,
        job_id: &str,
        path: &str,
        file: &File,
        len: u64,
        hash: &Sha256,
        etag: Option<&str>,
    ) -> Result<bool, LibraryError> {
        file.sync_all()?;
        self.conn.execute("UPDATE tree_files SET progress=?3,checkpoint_hash=?4,etag=?5 WHERE job_id=?1 AND path=?2",params![job_id,path,len as i64,format!("{:x}",hash.clone().finalize()),etag])?;
        self.conn.execute("UPDATE download_jobs SET progress=(SELECT COALESCE(sum(progress),0) FROM tree_files WHERE job_id=?1),updated_at=?2 WHERE id=?1",params![job_id,now()?])?;
        Ok(matches!(
            self.job(job_id)?.state.as_str(),
            "Paused" | "Cancelled"
        ))
    }

    fn prepare_tree_stage(
        &mut self,
        job_id: &str,
        path: &str,
        file_id: &str,
        size: u64,
    ) -> Result<(File, u64, Sha256, Option<String>), LibraryError> {
        let stage = child_stage(self, job_id, file_id)?;
        private_dir(stage.parent().unwrap())?;
        if !safe_regular(&stage)? {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&stage)?
                .sync_all()?;
            sync_dir(stage.parent().unwrap())?;
        }
        let mut file = open_regular(&stage, true)?;
        let (progress, checkpoint, etag): (i64, Option<String>, Option<String>) =
            self.conn.query_row(
                "SELECT progress,checkpoint_hash,etag FROM tree_files WHERE job_id=?1 AND path=?2",
                params![job_id, path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        let mut len = file.metadata()?.len();
        let mut hash = Sha256::new();
        let mut validator = etag;
        if progress < 0 || progress as u64 > size || len < progress as u64 {
            file.set_len(0)?;
            len = 0;
            validator = None;
        } else if len > progress as u64 {
            file.set_len(progress as u64)?;
            len = progress as u64;
        }
        file.seek(SeekFrom::Start(0))?;
        let mut buffer = [0u8; 65536];
        let mut remaining = len;
        while remaining > 0 {
            let limit = remaining.min(buffer.len() as u64) as usize;
            let count = file.read(&mut buffer[..limit])?;
            if count == 0 {
                return Err(invalid("staging file shortened during validation"));
            }
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if len > 0
            && checkpoint.as_deref() != Some(format!("{:x}", hash.clone().finalize()).as_str())
        {
            file.set_len(0)?;
            len = 0;
            hash = Sha256::new();
            validator = None;
        }
        if len != progress as u64 {
            self.tree_checkpoint(job_id, path, &file, len, &hash, None)?;
        }
        file.seek(SeekFrom::Start(len))?;
        Ok((file, len, hash, validator))
    }

    fn obtain_tree_file(
        &mut self,
        client: &ApiClient,
        job_id: &str,
        entry: &InventoryEntry,
        pause_after: Option<u64>,
    ) -> Result<bool, LibraryError> {
        let InventoryEntry::File {
            path,
            file_id,
            size,
            sha256,
            ..
        } = entry
        else {
            return Ok(false);
        };
        let job = self.job(job_id)?;
        let object = self.object(&format!("adfile:sha256:{sha256}"))?;
        if safe_regular(&object)? {
            if hash_file(&object, *size)? != *sha256 {
                return Err(invalid("existing object conflicts with Tree file identity"));
            }
            self.conn.execute(
                "UPDATE tree_files SET verified=1,progress=size WHERE job_id=?1 AND path=?2",
                params![job_id, path],
            )?;
            self.conn.execute("UPDATE download_jobs SET progress=(SELECT COALESCE(sum(progress),0) FROM tree_files WHERE job_id=?1),updated_at=?2 WHERE id=?1",params![job_id,now()?])?;
            return Ok(false);
        }
        let free = fs2::available_space(&self.paths.content)?;
        if free
            < size.saturating_sub(
                self.tree_files(job_id)?
                    .iter()
                    .find(|f| f.path == *path)
                    .map_or(0, |f| f.progress),
            )
        {
            return Err(invalid("insufficient content-root space"));
        }
        let (mut file, offset, hash, etag) =
            self.prepare_tree_stage(job_id, path, file_id, *size)?;
        let local_pause = pause_after.map(|remaining| offset.saturating_add(remaining));
        if offset == *size {
            if format!("{:x}", hash.finalize()) != *sha256 {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                self.tree_checkpoint(job_id, path, &file, 0, &Sha256::new(), None)?;
                return Err(invalid("SHA-256 mismatch"));
            }
        } else {
            let mut retries = 0;
            let mut first = Some((file, offset, hash, etag));
            loop {
                let (mut file, offset, hash, etag) = if let Some(value) = first.take() {
                    value
                } else {
                    self.prepare_tree_stage(job_id, path, file_id, *size)?
                };
                let result = (|| {
                    let response = client.tree_file_content(
                        &job.artifact_id,
                        file_id,
                        (offset > 0).then_some(offset),
                        etag.as_deref(),
                    )?;
                    receive_blob(
                        response,
                        &mut file,
                        offset,
                        hash,
                        BlobSpec {
                            expected_size: *size,
                            expected_digest: sha256,
                            prior_etag: etag.as_deref(),
                            pause_after: local_pause,
                        },
                        |f, n, h, e| self.tree_checkpoint(job_id, path, f, n, h, e),
                    )
                })();
                match result {
                    Ok(BlobOutcome::Complete) => break,
                    Ok(BlobOutcome::Stopped) => {
                        if self.job(job_id)?.state == "Downloading" {
                            self.state(job_id, "Paused", None)?;
                        }
                        return Ok(true);
                    }
                    Err(error)
                        if (matches!(error, LibraryError::Api(ApiError::Transport(_)))
                            || matches!(
                                error,
                                LibraryError::Api(ApiError::Http {
                                    status: 429 | 502 | 503 | 504,
                                    ..
                                })
                            ))
                            && retries < 3 =>
                    {
                        retries += 1;
                        thread::sleep(Duration::from_millis(100 * retries));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let stage = child_stage(self, job_id, file_id)?;
        if hash_file(&stage, *size)? != *sha256 {
            return Err(invalid("staging identity mismatch"));
        }
        private_dir(object.parent().unwrap())?;
        open_regular(&stage, false)?.sync_all()?;
        match fs::hard_link(&stage, &object) {
            Ok(()) => sync_dir(object.parent().unwrap())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if hash_file(&object, *size)? != *sha256 {
                    return Err(invalid("existing object conflicts with Tree file identity"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        let mut permissions = fs::metadata(&object)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&object, permissions)?;
        open_regular(&object, false)?.sync_all()?;
        self.conn.execute(
            "UPDATE tree_files SET verified=1,progress=size WHERE job_id=?1 AND path=?2",
            params![job_id, path],
        )?;
        self.conn.execute("UPDATE download_jobs SET progress=(SELECT COALESCE(sum(progress),0) FROM tree_files WHERE job_id=?1),updated_at=?2 WHERE id=?1",params![job_id,now()?])?;
        Ok(false)
    }

    fn verify_tree_directory(
        &self,
        root: &Path,
        entries: &[InventoryEntry],
    ) -> Result<(), LibraryError> {
        existing_dir(root)?;
        let mut expected = std::collections::HashSet::new();
        for entry in entries {
            let path = entry.path();
            expected.insert(path.to_string());
            let target = root.join(path);
            match entry {
                InventoryEntry::Directory { .. } => existing_dir(&target)?,
                InventoryEntry::File {
                    size,
                    sha256,
                    executable,
                    ..
                } => {
                    if hash_file(&target, *size)? != *sha256 {
                        return Err(invalid(format!("Tree file verification failed: {path}")));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let has_exec =
                            fs::symlink_metadata(&target)?.permissions().mode() & 0o111 != 0;
                        if has_exec != *executable {
                            return Err(invalid(format!("Tree executable flag mismatch: {path}")));
                        }
                    }
                    #[cfg(not(unix))]
                    let _ = executable;
                }
            }
        }
        fn scan(
            root: &Path,
            relative: &str,
            expected: &std::collections::HashSet<String>,
        ) -> Result<(), LibraryError> {
            for child in fs::read_dir(root)? {
                let child = child?;
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("Tree contains unrepresentable name"))?;
                let path = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                if !expected.contains(&path) {
                    return Err(invalid(format!(
                        "unexpected Tree destination entry: {path}"
                    )));
                }
                let meta = fs::symlink_metadata(child.path())?;
                if meta.file_type().is_symlink() {
                    return Err(invalid("Tree contains symlink"));
                }
                if meta.is_dir() {
                    scan(&child.path(), &path, expected)?;
                } else if !meta.is_file() {
                    return Err(invalid("Tree contains special file"));
                }
            }
            Ok(())
        }
        scan(root, "", &expected)
    }

    fn prepare_tree_materialization(
        &self,
        job: &DownloadJob,
        entries: &[InventoryEntry],
    ) -> Result<PathBuf, LibraryError> {
        if identity(entries)? != job.artifact_id {
            return Err(invalid("Tree identity mismatch before materialization"));
        }
        validate_destination(entries)?;
        let stage = stage_root(self, &job.id)?;
        private_dir(&stage)?;
        let build = stage.join("tree.build");
        let ready = stage.join("tree.ready");
        if fs::symlink_metadata(&ready).is_ok() {
            self.verify_tree_directory(&ready, entries)?;
            return Ok(ready);
        }
        if fs2::available_space(&self.paths.content)? < job.expected_size {
            return Err(invalid(
                "insufficient content-root space for Tree materialization",
            ));
        }
        if fs::symlink_metadata(&build).is_ok() {
            // A previous build is private job staging. Reject unknown or symlinked entries before cleanup.
            let mut allowed = std::collections::HashSet::new();
            for entry in entries {
                allowed.insert(entry.path().to_string());
            }
            fn check(
                path: &Path,
                prefix: &str,
                allowed: &std::collections::HashSet<String>,
            ) -> Result<(), LibraryError> {
                existing_dir(path)?;
                for item in fs::read_dir(path)? {
                    let item = item?;
                    let name = item
                        .file_name()
                        .into_string()
                        .map_err(|_| invalid("unsafe staging name"))?;
                    let rel = if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    };
                    if !allowed.contains(&rel) {
                        return Err(invalid("unexpected interrupted Tree staging entry"));
                    }
                    let meta = fs::symlink_metadata(item.path())?;
                    if meta.file_type().is_symlink() {
                        return Err(invalid("symlink in interrupted Tree staging"));
                    }
                    if meta.is_dir() {
                        check(&item.path(), &rel, allowed)?;
                    } else if !meta.is_file() {
                        return Err(invalid("special file in interrupted Tree staging"));
                    }
                }
                Ok(())
            }
            check(&build, "", &allowed)?;
            #[cfg(unix)]
            {
                let root = open_dir_nofollow(&build)?;
                for entry in entries {
                    if let InventoryEntry::File { path, .. } = entry {
                        unlink_beneath(&root, path, false)?;
                    }
                }
                let mut dirs: Vec<&str> = entries
                    .iter()
                    .filter_map(|entry| {
                        if let InventoryEntry::Directory { path } = entry {
                            Some(path.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                dirs.sort_by(|a, b| b.as_bytes().cmp(a.as_bytes()));
                for path in dirs {
                    unlink_beneath(&root, path, true)?;
                }
                root.sync_all()?;
                fs::remove_dir(&build)?;
            }
            #[cfg(not(unix))]
            fs::remove_dir_all(&build)?;
            sync_dir(&stage)?;
        }
        private_dir(&build)?;
        #[cfg(unix)]
        let root_handle = open_dir_nofollow(&build)?;
        for entry in entries {
            if let InventoryEntry::Directory { path } = entry {
                #[cfg(unix)]
                {
                    dir_beneath(&root_handle, path, true)?;
                }
                #[cfg(not(unix))]
                {
                    private_dir(&build.join(path))?;
                }
            }
        }
        for entry in entries {
            if let InventoryEntry::File {
                path,
                size,
                sha256,
                executable,
                ..
            } = entry
            {
                let object = self.object(&format!("adfile:sha256:{sha256}"))?;
                let mut source = open_regular(&object, false)?;
                #[cfg(unix)]
                let mut output = create_file_beneath(&root_handle, path)?;
                #[cfg(not(unix))]
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(build.join(path))?;
                let mut hash = Sha256::new();
                let mut length = 0u64;
                let mut buffer = [0u8; 65536];
                loop {
                    let count = source.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    output.write_all(&buffer[..count])?;
                    hash.update(&buffer[..count]);
                    length = length
                        .checked_add(count as u64)
                        .ok_or_else(|| invalid("Tree file size overflow"))?;
                    if length > *size {
                        return Err(invalid("Tree object size changed during copy"));
                    }
                }
                if length != *size || format!("{:x}", hash.finalize()) != *sha256 {
                    return Err(invalid("Tree object changed during copy"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if *executable { 0o500 } else { 0o400 };
                    output.set_permissions(fs::Permissions::from_mode(mode))?;
                }
                #[cfg(not(unix))]
                let _ = executable;
                output.sync_all()?;
            }
        }
        // Sync directories deepest first so the complete structure is durable before promotion.
        for entry in entries.iter().rev() {
            if let InventoryEntry::Directory { path } = entry {
                #[cfg(unix)]
                {
                    dir_beneath(&root_handle, path, false)?.sync_all()?;
                }
                #[cfg(not(unix))]
                {
                    sync_dir(&build.join(path))?;
                }
            }
        }
        sync_dir(&build)?;
        self.verify_tree_directory(&build, entries)?;
        rename_tree_noreplace(&build, &ready)?;
        sync_dir(&stage)?;
        Ok(ready)
    }

    fn complete_tree(
        &mut self,
        job: &DownloadJob,
        entries: &[InventoryEntry],
    ) -> Result<(), LibraryError> {
        let destination = tree_destination(self, job)?;
        let parent = destination.parent().unwrap();
        private_dir(parent.parent().unwrap())?;
        private_dir(parent)?;
        if fs::symlink_metadata(&destination).is_ok() {
            self.verify_tree_directory(&destination, entries)?;
        } else {
            let ready = self.prepare_tree_materialization(job, entries)?;
            if let Err(error) = rename_tree_noreplace(&ready, &destination)
                && !matches!(&error,LibraryError::Io(io) if io.kind()==std::io::ErrorKind::AlreadyExists)
            {
                return Err(error);
            }
            sync_dir(parent)?;
            self.verify_tree_directory(&destination, entries)?;
        }
        let relative = destination
            .strip_prefix(&self.paths.content)
            .map_err(|_| invalid("unsafe Tree path"))?
            .to_string_lossy()
            .into_owned();
        let tx = self.conn.transaction()?;
        tx.execute("INSERT OR IGNORE INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,?4,?5)",params![job.profile_id.0,job.artifact_id,relative,now()?,job.association_id])?;
        if tx.execute("UPDATE download_jobs SET state='Completed',progress=expected_size,last_error=NULL,updated_at=?2 WHERE id=?1 AND state='Committing'",params![job.id,now()?])?!=1 {return Err(invalid("Tree job left Committing state"));}
        tx.commit()?;
        Ok(())
    }

    pub fn run_tree_job(
        &mut self,
        client: &ApiClient,
        id: &str,
        pause_after: Option<u64>,
    ) -> Result<DownloadJob, LibraryError> {
        let _lock = self.lock_job(id)?;
        let job = self.job(id)?;
        if !job.artifact_id.starts_with("adtree:v1:sha256:") {
            return Err(invalid("not a Tree job"));
        }
        let profile = self.profile(&job.profile_id)?;
        if profile.archived || profile.origin != client.origin() {
            return Err(invalid("active profile and client origin must match"));
        }
        if job.state == "Committing" {
            let entries = self
                .tree_inventory(id)?
                .ok_or_else(|| invalid("Tree inventory missing"))?;
            self.complete_tree(&job, &entries)?;
            return self.job(id);
        }
        if job.state != "Queued" {
            return Err(invalid("job must be queued before running"));
        }
        let result = (|| {
            let artifact = client.artifact(&job.artifact_id)?;
            if artifact.kind != "Tree" {
                return Err(invalid("remote Tree Artifact metadata changed"));
            }
            let entries = self.fetch_tree_inventory(client, &job)?;
            if matches!(self.job(id)?.state.as_str(), "Queued" | "FetchingInventory") {
                self.state(id, "Downloading", None)?;
            }
            self.conn.execute(
                "UPDATE download_jobs SET attempts=attempts+1,updated_at=?2 WHERE id=?1",
                params![id, now()?],
            )?;
            for entry in &entries {
                if self.job(id)?.state != "Downloading" {
                    return self.job(id);
                }
                let already = self
                    .tree_files(id)?
                    .iter()
                    .find(|f| f.path == entry.path())
                    .is_some_and(|f| f.verified);
                if already
                    && let InventoryEntry::File {
                        path, size, sha256, ..
                    } = entry
                {
                    let object = self.object(&format!("adfile:sha256:{sha256}"))?;
                    if safe_regular(&object)? && hash_file(&object, *size)? == *sha256 {
                        continue;
                    }
                    self.conn.execute(
                        "UPDATE tree_files SET verified=0 WHERE job_id=?1 AND path=?2",
                        params![id, path],
                    )?;
                }
                let limit = pause_after
                    .map(|limit| limit.saturating_sub(self.job(id).map_or(0, |j| j.progress)));
                if limit == Some(0) {
                    self.state(id, "Paused", None)?;
                    return self.job(id);
                }
                if self.obtain_tree_file(client, id, entry, limit)? {
                    return self.job(id);
                }
            }
            if self
                .tree_files(id)?
                .iter()
                .any(|f| !f.verified || f.progress != f.size)
            {
                return Err(invalid("Tree member incomplete"));
            }
            self.transition(id, "Downloading", "Verifying")?;
            if identity(&entries)? != job.artifact_id {
                return Err(invalid("Tree identity mismatch"));
            }
            self.transition(id, "Verifying", "Committing")?;
            self.complete_tree(&self.job(id)?, &entries)?;
            self.job(id)
        })();
        if let Err(error) = &result
            && matches!(
                self.job(id)?.state.as_str(),
                "Queued" | "FetchingInventory" | "Downloading" | "Verifying"
            )
        {
            self.state(id, "Failed", Some(&error.to_string()))?;
        }
        result
    }

    pub fn reconcile_tree_jobs(&mut self) -> Result<(), LibraryError> {
        let ids:Vec<String>=self.conn.prepare("SELECT id FROM download_jobs WHERE artifact_id LIKE 'adtree:v1:sha256:%' AND state IN ('FetchingInventory','Downloading','Verifying','Committing')")?.query_map([],|r|r.get(0))?.collect::<Result<_,_>>()?;
        for id in ids {
            let _lock = match self.lock_job(&id) {
                Ok(lock) => lock,
                Err(LibraryError::Invalid(message))
                    if message == "download job is active in another process" =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let job = self.job(&id)?;
            if job.state == "Committing" {
                let entries = self
                    .tree_inventory(&id)?
                    .ok_or_else(|| invalid("Tree inventory missing"))?;
                let result = self.complete_tree(&job, &entries);
                if result.is_err() {
                    self.state(&id, "Failed", Some("Tree commit recovery needs inspection"))?;
                }
            } else {
                self.state(
                    &id,
                    "Paused",
                    Some("interrupted; resume to validate Tree staging"),
                )?;
            }
        }
        Ok(())
    }

    pub fn verified_trees(
        &self,
        profile: Option<&ServerProfileId>,
    ) -> Result<Vec<VerifiedTree>, LibraryError> {
        let mut stmt=self.conn.prepare("SELECT profile_id,artifact_id,association_id,verified_at FROM verified_content WHERE artifact_id LIKE 'adtree:v1:sha256:%' AND (?1 IS NULL OR profile_id=?1) ORDER BY verified_at")?;
        let rows = stmt
            .query_map([profile.map(|p| p.0.as_str())], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(|(profile_id,artifact_id,association_id,verified_at)|{
            let association_id=association_id.ok_or_else(||invalid("verified Tree lacks Association"))?;
            let job_id:String=self.conn.query_row("SELECT id FROM download_jobs WHERE profile_id=?1 AND artifact_id=?2 AND association_id=?3 AND state='Completed' ORDER BY updated_at DESC LIMIT 1",params![profile_id,artifact_id,association_id],|r|r.get(0))?;
            let entries=self.tree_inventory(&job_id)?.ok_or_else(||invalid("verified Tree inventory missing"))?;
            if identity(&entries)?!=artifact_id{return Err(invalid("verified Tree identity mismatch"));}
            let job=self.job(&job_id)?;
            let path=tree_destination(self,&job)?;
            self.verify_tree_directory(&path,&entries)?;
            Ok(VerifiedTree{profile_id:ServerProfileId(profile_id),association_id,artifact_id,path,file_count:entries.iter().filter(|e|matches!(e,InventoryEntry::File{..})).count(),verified_at})
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryPaths;
    const APP: &str = "11111111-1111-4111-8111-111111111111";
    const RELEASE: &str = "22222222-2222-4222-8222-222222222222";
    const ASSOCIATION: &str = "33333333-3333-4333-8333-333333333333";
    fn entries() -> Vec<InventoryEntry> {
        vec![
            InventoryEntry::File {
                path: "a.txt".into(),
                file_id: Uuid::new_v4().to_string(),
                size: 1,
                sha256: format!("{:x}", Sha256::digest(b"A")),
                executable: false,
            },
            InventoryEntry::Directory { path: "bin".into() },
            InventoryEntry::File {
                path: "bin/run".into(),
                file_id: Uuid::new_v4().to_string(),
                size: 1,
                sha256: format!("{:x}", Sha256::digest(b"B")),
                executable: true,
            },
            InventoryEntry::Directory {
                path: "empty".into(),
            },
            InventoryEntry::Directory {
                path: "empty/nested".into(),
            },
        ]
    }
    fn fixture() -> (tempfile::TempDir, Library, DownloadJob, Vec<InventoryEntry>) {
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::open(LibraryPaths {
            metadata: root.path().join("metadata"),
            content: root.path().join("content"),
        })
        .unwrap();
        let profile = library
            .add_profile("test", "https://example.com", false)
            .unwrap();
        let entries = entries();
        let artifact = identity(&entries).unwrap();
        let job_id = Uuid::new_v4().to_string();
        let tx = library.conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
            params![profile.id.0, APP],
        )
        .unwrap();
        tx.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.id.0,RELEASE,APP]).unwrap();
        tx.execute(
            "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
            params![profile.id.0, artifact],
        )
        .unwrap();
        tx.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,'{}',0)",params![profile.id.0,ASSOCIATION,RELEASE,artifact]).unwrap();
        tx.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,association_id,state,created_at,updated_at,expected_size,inventory_json,inventory_etag) VALUES (?1,?2,?3,?4,'Committing',0,0,2,?5,'\"test\"')",params![job_id,profile.id.0,artifact,ASSOCIATION,serde_json::to_string(&entries).unwrap()]).unwrap();
        tx.commit().unwrap();
        let job = library.job(&job_id).unwrap();
        for (digest, bytes) in [
            (format!("{:x}", Sha256::digest(b"A")), b"A".as_slice()),
            (format!("{:x}", Sha256::digest(b"B")), b"B".as_slice()),
        ] {
            let object = library.object(&format!("adfile:sha256:{digest}")).unwrap();
            private_dir(object.parent().unwrap()).unwrap();
            fs::write(&object, bytes).unwrap();
        }
        (root, library, job, entries)
    }
    #[test]
    fn atomic_tree_completion_and_offline_verification() {
        let (_root, mut library, job, entries) = fixture();
        library.complete_tree(&job, &entries).unwrap();
        assert_eq!(library.job(&job.id).unwrap().state, "Completed");
        let visible = library.verified_trees(Some(&job.profile_id)).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].file_count, 2);
        assert!(visible[0].path.join("empty/nested").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(visible[0].path.join("bin/run"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        drop(library);
        let library = Library::open(LibraryPaths {
            metadata: _root.path().join("metadata"),
            content: _root.path().join("content"),
        })
        .unwrap();
        assert_eq!(library.verified_trees(None).unwrap().len(), 1);
    }
    #[test]
    fn interrupted_committing_recovers_without_network() {
        let (_root, mut library, job, entries) = fixture();
        let ready = library
            .prepare_tree_materialization(&job, &entries)
            .unwrap();
        let destination = tree_destination(&library, &job).unwrap();
        private_dir(destination.parent().unwrap().parent().unwrap()).unwrap();
        private_dir(destination.parent().unwrap()).unwrap();
        fs::rename(&ready, &destination).unwrap();
        library.reconcile_tree_jobs().unwrap();
        assert_eq!(library.job(&job.id).unwrap().state, "Completed");
        assert_eq!(library.verified_trees(None).unwrap().len(), 1);
    }
    #[test]
    fn missing_or_unsafe_content_never_completes() {
        let (_root, mut library, job, entries) = fixture();
        let digest = format!("{:x}", Sha256::digest(b"B"));
        let object = library.object(&format!("adfile:sha256:{digest}")).unwrap();
        fs::write(&object, b"wrong").unwrap();
        assert!(library.complete_tree(&job, &entries).is_err());
        assert_ne!(library.job(&job.id).unwrap().state, "Completed");
        assert!(library.verified_trees(None).unwrap().is_empty());
        fs::write(&object, b"B").unwrap();
        #[cfg(unix)]
        {
            let build = stage_root(&library, &job.id).unwrap().join("tree.build");
            let link = build.join("bin");
            if link.exists() {
                fs::remove_file(link.join("run")).unwrap();
                fs::remove_dir(&link).unwrap();
            }
            std::os::unix::fs::symlink("/tmp", &link).unwrap();
            assert!(library.complete_tree(&job, &entries).is_err());
            assert_ne!(library.job(&job.id).unwrap().state, "Completed");
        }
    }

    #[test]
    fn same_tree_content_keeps_two_profile_references() {
        let (_root, mut library, first, entries) = fixture();
        library.complete_tree(&first, &entries).unwrap();
        let second_profile = library
            .add_profile("second", "https://example.com", false)
            .unwrap();
        let second_id = Uuid::new_v4().to_string();
        let tx = library.conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
            params![second_profile.id.0, APP],
        )
        .unwrap();
        tx.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![second_profile.id.0,RELEASE,APP]).unwrap();
        tx.execute(
            "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
            params![second_profile.id.0, first.artifact_id],
        )
        .unwrap();
        tx.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,'{}',0)",params![second_profile.id.0,ASSOCIATION,RELEASE,first.artifact_id]).unwrap();
        tx.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,association_id,state,created_at,updated_at,expected_size,inventory_json,inventory_etag) VALUES (?1,?2,?3,?4,'Committing',0,0,2,?5,'\"test\"')",params![second_id,second_profile.id.0,first.artifact_id,ASSOCIATION,serde_json::to_string(&entries).unwrap()]).unwrap();
        tx.commit().unwrap();
        let second = library.job(&second_id).unwrap();
        library.complete_tree(&second, &entries).unwrap();
        let trees = library.verified_trees(None).unwrap();
        assert_eq!(trees.len(), 2);
        assert_ne!(trees[0].path, trees[1].path);
        assert_eq!(trees[0].artifact_id, trees[1].artifact_id);
    }

    #[test]
    fn failed_database_commit_cannot_mark_tree_completed() {
        let (_root, mut library, job, entries) = fixture();
        library
            .conn
            .execute_batch("DROP TABLE verified_content")
            .unwrap();
        assert!(library.complete_tree(&job, &entries).is_err());
        assert_eq!(library.job(&job.id).unwrap().state, "Committing");
    }

    #[test]
    fn tree_job_lock_blocks_concurrent_run() {
        let (_root, mut library, job, _entries) = fixture();
        let lock = library.lock_job(&job.id).unwrap();
        let client = ApiClient::new("https://example.com", None, false).unwrap();
        assert!(library.run_tree_job(&client, &job.id, None).is_err());
        drop(lock);
    }
}
