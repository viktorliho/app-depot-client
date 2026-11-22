//! Durable, verified single-file Artifact transfer. Paths are derived only from local UUIDs and
//! validated content identities; Server-supplied names never become filesystem paths.
use crate::{
    ServerProfileId,
    api::{ApiClient, ApiError},
    library::{Library, LibraryError, now},
};
use fs2::FileExt;
use rusqlite::{OptionalExtension, params};
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

const CHUNK: usize = 64 * 1024;
const CHECKPOINT: u64 = 4 * 1024 * 1024;

/// Ephemeral observations. They are never recovery checkpoints or proof of verification.
#[derive(Clone, Debug)]
pub struct TransferObservation {
    pub phase: TransferPhase,
    pub network_bytes: u64,
    pub received_bytes: u64,
    pub current_file: Option<String>,
    pub files_done: usize,
    pub files_total: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPhase {
    PreparingInventory,
    WaitingForServer,
    Downloading,
    Verifying,
    PreparingLocalFiles,
    SavingVerifiedContent,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJob {
    pub id: String,
    pub profile_id: ServerProfileId,
    pub association_id: String,
    pub artifact_id: String,
    pub state: String,
    pub expected_size: u64,
    pub progress: u64,
    pub etag: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub attempts: u32,
    pub queue_rank: i64,
    pub auto_resume: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedFile {
    pub profile_id: ServerProfileId,
    pub association_id: String,
    pub artifact_id: String,
    pub path: PathBuf,
    pub size: u64,
    pub verified_at: i64,
}

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}
fn digest(id: &str) -> Result<&str, LibraryError> {
    let value = id
        .strip_prefix("adfile:sha256:")
        .ok_or_else(|| invalid("not a File Artifact identity"))?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid File Artifact digest"));
    }
    Ok(value)
}
pub(crate) fn strong_etag(value: &str) -> bool {
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value[1..value.len() - 1].contains('"')
        && value.bytes().all(|byte| (33..=126).contains(&byte))
}
pub(crate) fn safe_regular(path: &Path) -> Result<bool, LibraryError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(invalid(format!("unsafe existing file: {}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
pub(crate) fn open_regular(path: &Path, write: bool) -> Result<File, LibraryError> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("unsafe non-regular content file"));
    }
    Ok(file)
}
#[cfg(unix)]
fn same_inode(first: &Path, second: &Path) -> Result<bool, LibraryError> {
    use std::os::unix::fs::MetadataExt;
    let a = open_regular(first, false)?.metadata()?;
    let b = open_regular(second, false)?.metadata()?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
#[cfg(not(unix))]
fn same_inode(_first: &Path, _second: &Path) -> Result<bool, LibraryError> {
    Ok(false)
}
pub(crate) fn private_dir(path: &Path) -> Result<(), LibraryError> {
    if path.exists() {
        if fs::symlink_metadata(path)?.is_dir() {
            return Ok(());
        }
        return Err(invalid("unsafe content directory"));
    }
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    sync_dir(
        path.parent()
            .ok_or_else(|| invalid("directory has no parent"))?,
    )?;
    Ok(())
}
pub(crate) fn existing_dir(path: &Path) -> Result<(), LibraryError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(invalid("unsafe content directory"));
    }
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<(), LibraryError> {
    File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn hash_file(path: &Path, expected_size: u64) -> Result<String, LibraryError> {
    if !safe_regular(path)? {
        return Err(invalid("verified file is missing"));
    }
    let mut file = open_regular(path, false)?;
    if file.metadata()?.len() != expected_size {
        return Err(invalid("verified file has wrong size"));
    }
    let mut hash = Sha256::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (
        start.parse::<u64>().ok()?,
        end.parse::<u64>().ok()?,
        total.parse::<u64>().ok()?,
    );
    (start <= end && end < total).then_some((start, end, total))
}

pub(crate) enum BlobOutcome {
    Complete,
    Stopped,
}

pub(crate) struct BlobSpec<'a> {
    pub expected_size: u64,
    pub expected_digest: &'a str,
    pub prior_etag: Option<&'a str>,
    pub pause_after: Option<u64>,
    pub checkpoint_on_complete: bool,
}

/// Shared bounded Range/If-Range receiver for standalone and Tree member files.
pub(crate) fn receive_blob(
    response: ureq::Response,
    file: &mut File,
    mut offset: u64,
    mut hash: Sha256,
    spec: BlobSpec<'_>,
    mut checkpoint: impl FnMut(&File, u64, &Sha256, Option<&str>) -> Result<bool, LibraryError>,
    mut observe_bytes: impl FnMut(u64, u64),
) -> Result<BlobOutcome, LibraryError> {
    let BlobSpec {
        expected_size,
        expected_digest,
        prior_etag,
        pause_after,
        checkpoint_on_complete,
    } = spec;
    let status = response.status();
    if status == 416 {
        if response.header("Content-Range") != Some(format!("bytes */{expected_size}").as_str()) {
            return Err(invalid("invalid 416 Content-Range"));
        }
        file.set_len(0)?;
        hash = Sha256::new();
        checkpoint(file, 0, &hash, None)?;
        return Err(LibraryError::Api(ApiError::Transport(
            "range rejected; restarting from zero".into(),
        )));
    }
    let etag = response
        .header("ETag")
        .ok_or_else(|| invalid("download ETag missing"))?
        .to_owned();
    if !strong_etag(&etag) {
        return Err(invalid("download ETag is not strong"));
    }
    if status == 206 {
        if offset == 0 || prior_etag != Some(etag.as_str()) {
            return Err(invalid("unexpected or changed 206 ETag"));
        }
        if response.header("Content-Range").and_then(content_range)
            != Some((offset, expected_size - 1, expected_size))
        {
            return Err(invalid("invalid Content-Range"));
        }
    } else if status == 200 {
        if response.header("Content-Range").is_some() {
            return Err(invalid("unexpected Content-Range on full response"));
        }
        if offset > 0 {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            offset = 0;
            hash = Sha256::new();
            checkpoint(file, 0, &hash, Some(&etag))?;
        }
    } else {
        return Err(invalid(format!("unexpected download status {status}")));
    }
    let length = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| invalid("download Content-Length missing or invalid"))?;
    if length != expected_size - offset {
        return Err(invalid("download Content-Length mismatch"));
    }
    if response
        .header("Content-Type")
        .unwrap_or("")
        .split(';')
        .next()
        != Some("application/octet-stream")
    {
        return Err(invalid("download Content-Type mismatch"));
    }
    let mut reader = response.into_reader();
    let mut buf = [0u8; CHUNK];
    let mut since = 0u64;
    while offset < expected_size {
        let count = reader
            .read(&mut buf[..((expected_size - offset).min(CHUNK as u64) as usize)])
            .map_err(|e| LibraryError::Api(ApiError::Transport(e.to_string())))?;
        if count == 0 {
            return Err(LibraryError::Api(ApiError::Transport(
                "truncated download body".into(),
            )));
        }
        file.write_all(&buf[..count])?;
        hash.update(&buf[..count]);
        offset += count as u64;
        since += count as u64;
        observe_bytes(count as u64, offset);
        if since >= CHECKPOINT || pause_after.is_some_and(|limit| offset >= limit) {
            let stopped = checkpoint(file, offset, &hash, Some(&etag))?;
            since = 0;
            if stopped || pause_after.is_some_and(|limit| offset >= limit) {
                return Ok(BlobOutcome::Stopped);
            }
        }
    }
    if checkpoint_on_complete {
        checkpoint(file, offset, &hash, Some(&etag))?;
    }
    let mut extra = [0u8; 1];
    if reader
        .read(&mut extra)
        .map_err(|e| LibraryError::Api(ApiError::Transport(e.to_string())))?
        != 0
    {
        return Err(invalid("download body exceeds Content-Length"));
    }
    if format!("{:x}", hash.finalize()) != expected_digest {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        checkpoint(file, 0, &Sha256::new(), None)?;
        return Err(invalid("SHA-256 mismatch"));
    }
    Ok(BlobOutcome::Complete)
}

impl Library {
    pub fn active_association_job(
        &self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<Option<DownloadJob>, LibraryError> {
        let id: Option<String> = self.conn.query_row(
            "SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND state IN ('Queued','FetchingInventory','Downloading','Verifying','Committing','Paused') ORDER BY created_at DESC LIMIT 1",
            params![profile.0, association], |r| r.get(0),
        ).optional()?;
        id.map(|id| self.job(&id)).transpose()
    }

    pub fn next_queued_job(&self) -> Result<Option<DownloadJob>, LibraryError> {
        let id: Option<String> = self.conn.query_row(
            "SELECT j.id FROM download_jobs j JOIN profiles p ON p.id=j.profile_id WHERE j.state='Queued' AND p.archived=0 ORDER BY j.queue_rank DESC,j.created_at,j.id LIMIT 1",
            [], |r| r.get(0),
        ).optional()?;
        id.map(|id| self.job(&id)).transpose()
    }

    pub fn move_job_to_top(&mut self, id: &str) -> Result<DownloadJob, LibraryError> {
        let changed=self.conn.execute("UPDATE download_jobs SET queue_rank=(SELECT COALESCE(MAX(queue_rank),0)+1 FROM download_jobs),updated_at=?2 WHERE id=?1 AND state IN ('Queued','Paused')",params![id,now()?])?;
        if changed != 1 {
            return Err(invalid("job cannot be reprioritized"));
        }
        self.job(id)
    }

    pub fn preempt_job(&mut self, id: &str) -> Result<bool, LibraryError> {
        Ok(self.conn.execute("UPDATE download_jobs SET state='Paused',auto_resume=1,updated_at=?2 WHERE id=?1 AND state='Downloading'",params![id,now()?])? == 1)
    }

    pub fn restore_preempted_jobs(&mut self) -> Result<(), LibraryError> {
        // The paused transfer may still be flushing its last durable checkpoint in
        // another process. Requeue it only after that process releases the slot.
        let _slot = match self.lock_download_slot() {
            Ok(slot) => slot,
            Err(LibraryError::Invalid(message))
                if message == "another application download holds the library slot" =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.conn.execute("UPDATE download_jobs SET state='Queued',auto_resume=0 WHERE state='Paused' AND auto_resume=1",[])?;
        Ok(())
    }

    pub fn fail_queued_job(&mut self, id: &str, error: &str) -> Result<(), LibraryError> {
        self.conn.execute("UPDATE download_jobs SET state='Failed',last_error=?2,updated_at=?3 WHERE id=?1 AND state='Queued'",params![id,error,now()?])?;
        Ok(())
    }

    pub(crate) fn lock_download_slot(&self) -> Result<File, LibraryError> {
        let path = self.paths.metadata.join(".app-depot-download.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("unsafe download slot lock"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.metadata()?.permissions().mode() & 0o077 != 0 {
                return Err(invalid("download slot lock must be private"));
            }
        }
        file.try_lock_exclusive()
            .map_err(|_| invalid("another application download holds the library slot"))?;
        Ok(file)
    }

    pub(crate) fn lock_job(&self, id: &str) -> Result<File, LibraryError> {
        let stage = self.stage(id)?;
        let dir = stage
            .parent()
            .ok_or_else(|| invalid("invalid staging path"))?;
        private_dir(dir)?;
        let path = dir.join("job.lock");
        if !safe_regular(&path)? {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
                .sync_all()?;
            sync_dir(dir)?;
        }
        let file = open_regular(&path, true)?;
        file.try_lock_exclusive()
            .map_err(|_| invalid("download job is active in another process"))?;
        Ok(file)
    }
    fn stage(&self, id: &str) -> Result<PathBuf, LibraryError> {
        Uuid::parse_str(id).map_err(|_| invalid("invalid job ID"))?;
        Ok(self
            .paths
            .content
            .join("staging")
            .join(id)
            .join("file.part"))
    }
    pub(crate) fn object(&self, artifact_id: &str) -> Result<PathBuf, LibraryError> {
        let hash = digest(artifact_id)?;
        Ok(self
            .paths
            .content
            .join("objects")
            .join(&hash[..2])
            .join(hash))
    }
    fn materialized(&self, job: &DownloadJob) -> Result<PathBuf, LibraryError> {
        Uuid::parse_str(&job.profile_id.0).map_err(|_| invalid("invalid profile ID"))?;
        Uuid::parse_str(&job.association_id).map_err(|_| invalid("invalid association ID"))?;
        Ok(self
            .paths
            .content
            .join("artifacts")
            .join(&job.profile_id.0)
            .join(&job.association_id)
            .join(digest(&job.artifact_id)?))
    }
    pub fn queue_file(
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
        if let Some(job) = self.active_association_job(profile_id, association_id)? {
            return Ok(job);
        }
        let caps = client.capabilities()?;
        if caps.api_version != "1.0.0"
            || caps.file_identity != "adfile:sha256"
            || !caps.download_ranges
        {
            return Err(LibraryError::Api(ApiError::Incompatible(
                "File transfer capabilities are unsupported".into(),
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
        if artifact.kind != "File" {
            return Err(invalid("Tree Artifact belongs to M1.4"));
        }
        digest(&artifact.id)?;
        let size = artifact
            .size
            .ok_or_else(|| invalid("File Artifact size missing"))?;
        if size > i64::MAX as u64 {
            return Err(invalid("File Artifact exceeds supported local size"));
        }
        let id = Uuid::new_v4().to_string();
        let time = now()?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json",params![profile_id.0,app.id,serde_json::to_string(&app)?])?;
        tx.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,0) ON CONFLICT(profile_id,id) DO UPDATE SET app_id=excluded.app_id,metadata_json=excluded.metadata_json",params![profile_id.0,release.id,release.app_id,serde_json::to_string(&release)?])?;
        tx.execute("INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json",params![profile_id.0,artifact.id,serde_json::to_string(&artifact)?])?;
        tx.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5,0) ON CONFLICT(profile_id,id) DO UPDATE SET release_id=excluded.release_id,artifact_id=excluded.artifact_id,metadata_json=excluded.metadata_json",params![profile_id.0,association.id,association.release_id,association.artifact_id,serde_json::to_string(&association)?])?;
        if let Err(error) = tx.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,association_id,state,created_at,updated_at,expected_size,queue_rank) VALUES (?1,?2,?3,?4,'Queued',?5,?5,?6,(SELECT COALESCE(MAX(queue_rank),0)+1 FROM download_jobs))",params![id,profile_id.0,artifact.id,association.id,time,size as i64]) {
            drop(tx);
            if let Some(job) = self.active_association_job(profile_id, association_id)? {
                return Ok(job);
            }
            return Err(error.into());
        }
        tx.commit()?;
        self.job(&id)
    }
    pub fn job(&self, id: &str) -> Result<DownloadJob, LibraryError> {
        self.conn.query_row("SELECT profile_id,association_id,artifact_id,state,expected_size,progress,etag,last_error,created_at,updated_at,attempts,queue_rank,auto_resume FROM download_jobs WHERE id=?1",[id],|r| {
            let size: i64 = r.get(4)?; let progress: i64 = r.get(5)?; let attempts: i64 = r.get(10)?;
            Ok(DownloadJob { id:id.into(), profile_id:ServerProfileId(r.get(0)?),association_id:r.get::<_,Option<String>>(1)?.unwrap_or_default(),artifact_id:r.get(2)?,state:r.get(3)?,expected_size:size.max(0) as u64,progress:progress.max(0) as u64,etag:r.get(6)?,last_error:r.get(7)?,created_at:r.get(8)?,updated_at:r.get::<_,Option<i64>>(9)?.unwrap_or_default(),attempts:attempts.max(0) as u32,queue_rank:r.get(11)?,auto_resume:r.get::<_,i64>(12)? != 0 })
        }).optional()?.ok_or_else(||invalid("download job not found"))
    }
    pub fn jobs(
        &self,
        profile: Option<&ServerProfileId>,
    ) -> Result<Vec<DownloadJob>, LibraryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM download_jobs WHERE ?1 IS NULL OR profile_id=?1 ORDER BY queue_rank DESC,created_at,id",
        )?;
        let ids = stmt
            .query_map([profile.map(|p| p.0.as_str())], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter().map(|id| self.job(id)).collect()
    }
    pub fn pause_job(&mut self, id: &str) -> Result<DownloadJob, LibraryError> {
        let changed=self.conn.execute("UPDATE download_jobs SET state='Paused',auto_resume=0,updated_at=?2 WHERE id=?1 AND state IN ('Queued','FetchingInventory','Downloading','Failed')",params![id,now()?])?;
        if changed == 0 {
            return Err(invalid("job cannot be paused"));
        }
        self.job(id)
    }
    pub fn cancel_job(&mut self, id: &str) -> Result<DownloadJob, LibraryError> {
        let changed=self.conn.execute("UPDATE download_jobs SET state='Cancelled',updated_at=?2 WHERE id=?1 AND state IN ('Queued','FetchingInventory','Downloading','Paused','Failed')",params![id,now()?])?;
        if changed == 0 {
            return Err(invalid("job cannot be cancelled"));
        }
        self.job(id)
    }
    pub fn retry_job(&mut self, id: &str) -> Result<DownloadJob, LibraryError> {
        let changed=self.conn.execute("UPDATE download_jobs SET state='Queued',auto_resume=0,queue_rank=(SELECT COALESCE(MAX(queue_rank),0)+1 FROM download_jobs),last_error=NULL,updated_at=?2 WHERE id=?1 AND state IN ('Failed','Paused')",params![id,now()?])?;
        if changed == 0 {
            return Err(invalid("job cannot be resumed or retried"));
        }
        self.job(id)
    }
    pub(crate) fn state(
        &mut self,
        id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), LibraryError> {
        self.conn.execute(
            "UPDATE download_jobs SET state=?2,last_error=?3,updated_at=?4 WHERE id=?1",
            params![id, state, error, now()?],
        )?;
        Ok(())
    }
    pub(crate) fn transition(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, LibraryError> {
        Ok(self.conn.execute(
            "UPDATE download_jobs SET state=?3,updated_at=?4 WHERE id=?1 AND state=?2",
            params![id, from, to, now()?],
        )? == 1)
    }
    fn checkpoint(
        &mut self,
        id: &str,
        file: &File,
        len: u64,
        hash: &Sha256,
        etag: Option<&str>,
    ) -> Result<(), LibraryError> {
        file.sync_all()?;
        let value = format!("{:x}", hash.clone().finalize());
        self.conn.execute("UPDATE download_jobs SET progress=?2,checkpoint_hash=?3,etag=?4,updated_at=?5 WHERE id=?1",params![id,len as i64,value,etag,now()?])?;
        Ok(())
    }
    fn prepare_stage(&mut self, job: &DownloadJob) -> Result<(File, u64, Sha256), LibraryError> {
        let path = self.stage(&job.id)?;
        private_dir(
            path.parent()
                .ok_or_else(|| invalid("invalid staging path"))?,
        )?;
        if !safe_regular(&path)? {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
                .sync_all()?;
            sync_dir(path.parent().unwrap())?;
        }
        let mut file = open_regular(&path, true)?;
        let mut size = file.metadata()?.len();
        let mut hash = Sha256::new();
        let checkpoint: Option<String> = self.conn.query_row(
            "SELECT checkpoint_hash FROM download_jobs WHERE id=?1",
            [&job.id],
            |r| r.get(0),
        )?;
        if size < job.progress || job.progress > job.expected_size {
            file.set_len(0)?;
            size = 0;
        }
        if size > job.progress {
            file.set_len(job.progress)?;
            size = job.progress;
        }
        if size > 0 {
            file.seek(SeekFrom::Start(0))?;
            let mut remaining = size;
            let mut buf = [0u8; CHUNK];
            while remaining > 0 {
                let n = file.read(&mut buf[..remaining.min(CHUNK as u64) as usize])?;
                if n == 0 {
                    return Err(invalid("staging file shortened during validation"));
                }
                hash.update(&buf[..n]);
                remaining -= n as u64;
            }
            if checkpoint.as_deref() != Some(format!("{:x}", hash.clone().finalize()).as_str()) {
                file.set_len(0)?;
                size = 0;
                hash = Sha256::new();
            }
        }
        if size != job.progress {
            self.checkpoint(&job.id, &file, size, &hash, None)?;
        }
        file.seek(SeekFrom::Start(size))?;
        Ok((file, size, hash))
    }
    fn commit_file(&mut self, job: &DownloadJob) -> Result<(), LibraryError> {
        let object = self.object(&job.artifact_id)?;
        let stage = self.stage(&job.id)?;
        private_dir(object.parent().unwrap())?;
        let created_object = if safe_regular(&object)? {
            if hash_file(&object, job.expected_size)? != digest(&job.artifact_id)? {
                return Err(invalid("existing object conflicts with Artifact identity"));
            }
            false
        } else {
            if hash_file(&stage, job.expected_size)? != digest(&job.artifact_id)? {
                return Err(invalid("staging identity mismatch during commit"));
            }
            open_regular(&stage, false)?.sync_all()?;
            match fs::hard_link(&stage, &object) {
                Ok(()) => {
                    sync_dir(object.parent().unwrap())?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if hash_file(&object, job.expected_size)? != digest(&job.artifact_id)? {
                        return Err(invalid("existing object conflicts with Artifact identity"));
                    }
                    false
                }
                Err(error) => return Err(error.into()),
            }
        };
        if created_object
            && !same_inode(&stage, &object)?
            && hash_file(&object, job.expected_size)? != digest(&job.artifact_id)?
        {
            return Err(invalid("promoted object identity mismatch"));
        }
        let mut permissions = fs::metadata(&object)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&object, permissions)?;
        open_regular(&object, false)?.sync_all()?;
        let visible = self.materialized(job)?;
        let visible_parent = visible.parent().unwrap();
        private_dir(visible_parent.parent().unwrap())?;
        private_dir(visible_parent)?;
        let created_visible = if safe_regular(&visible)? {
            if hash_file(&visible, job.expected_size)? != digest(&job.artifact_id)? {
                return Err(invalid(
                    "existing materialization conflicts with Artifact identity",
                ));
            }
            false
        } else {
            match fs::hard_link(&object, &visible) {
                Ok(()) => {
                    sync_dir(visible_parent)?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if hash_file(&visible, job.expected_size)? != digest(&job.artifact_id)? {
                        return Err(invalid(
                            "existing materialization conflicts with Artifact identity",
                        ));
                    }
                    false
                }
                Err(error) => return Err(error.into()),
            }
        };
        if created_visible
            && !same_inode(&object, &visible)?
            && hash_file(&visible, job.expected_size)? != digest(&job.artifact_id)?
        {
            return Err(invalid("materialized file identity mismatch"));
        }
        let relative = visible
            .strip_prefix(&self.paths.content)
            .map_err(|_| invalid("invalid materialization path"))?
            .to_string_lossy()
            .into_owned();
        let tx = self.conn.transaction()?;
        tx.execute("INSERT OR IGNORE INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,?4,?5)",params![job.profile_id.0,job.artifact_id,relative,now()?,job.association_id])?;
        if tx.execute("UPDATE download_jobs SET state='Completed',progress=?2,last_error=NULL,updated_at=?3 WHERE id=?1 AND state='Committing'",params![job.id,job.expected_size as i64,now()?])?!=1 {return Err(invalid("job left Committing state"));}
        if safe_regular(&stage)? {
            fs::remove_file(&stage)?;
            sync_dir(stage.parent().unwrap())?;
        }
        tx.commit()?;
        Ok(())
    }
    fn transfer_once(
        &mut self,
        client: &ApiClient,
        id: &str,
        pause_after: Option<u64>,
        observer: &mut dyn FnMut(TransferObservation),
        network_bytes: &mut u64,
    ) -> Result<(), LibraryError> {
        let job = self.job(id)?;
        let (mut file, offset, hash) = self.prepare_stage(&job)?;
        if offset == job.expected_size {
            observer(TransferObservation {
                phase: TransferPhase::Verifying,
                network_bytes: *network_bytes,
                received_bytes: job.progress,
                current_file: None,
                files_done: 0,
                files_total: 1,
            });
            if !self.transition(id, "Downloading", "Verifying")? {
                return Ok(());
            }
            if format!("{:x}", hash.finalize()) != digest(&job.artifact_id)? {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                self.checkpoint(id, &file, 0, &Sha256::new(), None)?;
                return Err(invalid("SHA-256 mismatch"));
            }
            self.transition(id, "Verifying", "Committing")?;
            observer(TransferObservation {
                phase: TransferPhase::SavingVerifiedContent,
                network_bytes: *network_bytes,
                received_bytes: job.progress,
                current_file: None,
                files_done: 1,
                files_total: 1,
            });
            return self.commit_file(&job);
        }
        observer(TransferObservation {
            phase: TransferPhase::WaitingForServer,
            network_bytes: *network_bytes,
            received_bytes: job.progress,
            current_file: None,
            files_done: 0,
            files_total: 1,
        });
        let response = client.file_content(
            &job.artifact_id,
            (offset > 0).then_some(offset),
            job.etag.as_deref(),
        )?;
        let result = receive_blob(
            response,
            &mut file,
            offset,
            hash,
            BlobSpec {
                expected_size: job.expected_size,
                expected_digest: digest(&job.artifact_id)?,
                prior_etag: job.etag.as_deref(),
                pause_after,
                checkpoint_on_complete: true,
            },
            |file, len, hash, etag| {
                self.checkpoint(id, file, len, hash, etag)?;
                Ok(matches!(
                    self.job(id)?.state.as_str(),
                    "Paused" | "Cancelled"
                ))
            },
            |count, received| {
                *network_bytes = network_bytes.saturating_add(count);
                observer(TransferObservation {
                    phase: TransferPhase::Downloading,
                    network_bytes: *network_bytes,
                    received_bytes: received,
                    current_file: None,
                    files_done: 0,
                    files_total: 1,
                });
            },
        )?;
        if matches!(result, BlobOutcome::Stopped) {
            if self.job(id)?.state == "Downloading" {
                self.state(id, "Paused", None)?;
            }
            return Ok(());
        }
        if matches!(self.job(id)?.state.as_str(), "Paused" | "Cancelled") {
            return Ok(());
        }
        if !self.transition(id, "Downloading", "Verifying")? {
            return Ok(());
        }
        observer(TransferObservation {
            phase: TransferPhase::Verifying,
            network_bytes: *network_bytes,
            received_bytes: job.expected_size,
            current_file: None,
            files_done: 1,
            files_total: 1,
        });
        self.transition(id, "Verifying", "Committing")?;
        observer(TransferObservation {
            phase: TransferPhase::SavingVerifiedContent,
            network_bytes: *network_bytes,
            received_bytes: job.expected_size,
            current_file: None,
            files_done: 1,
            files_total: 1,
        });
        self.commit_file(&job)
    }
    pub fn run_file_job(
        &mut self,
        client: &ApiClient,
        id: &str,
        pause_after: Option<u64>,
    ) -> Result<DownloadJob, LibraryError> {
        self.run_file_job_observed(client, id, pause_after, &mut |_| {})
    }

    pub fn run_file_job_observed(
        &mut self,
        client: &ApiClient,
        id: &str,
        pause_after: Option<u64>,
        observer: &mut dyn FnMut(TransferObservation),
    ) -> Result<DownloadJob, LibraryError> {
        let _slot = self.lock_download_slot()?;
        let mut network_bytes = 0;
        let _lock = self.lock_job(id)?;
        let job = self.job(id)?;
        let profile = self.profile(&job.profile_id)?;
        if profile.archived || profile.origin != client.origin() {
            return Err(invalid("active profile and client origin must match"));
        }
        if job.state == "Committing" {
            self.commit_file(&job)?;
            return self.job(id);
        }
        if job.state != "Queued" {
            return Err(invalid("job must be queued before running"));
        }
        let artifact = match client.artifact(&job.artifact_id) {
            Ok(value) => value,
            Err(error) => {
                self.state(id, "Failed", Some(&error.to_string()))?;
                return Err(error.into());
            }
        };
        if artifact.kind != "File" || artifact.size != Some(job.expected_size) {
            self.state(id, "Failed", Some("remote Artifact metadata changed"))?;
            return Err(invalid("remote Artifact metadata changed"));
        }
        if self.conn.execute("UPDATE download_jobs SET state='Downloading',attempts=attempts+1,updated_at=?2 WHERE id=?1 AND state='Queued'",params![id,now()?])?!=1 { return Err(invalid("job is already running")); }
        let object = self.object(&job.artifact_id)?;
        if safe_regular(&object)? {
            if hash_file(&object, job.expected_size)? != digest(&job.artifact_id)? {
                self.state(
                    id,
                    "Failed",
                    Some("existing object conflicts with Artifact identity"),
                )?;
                return Err(invalid("existing object conflicts with Artifact identity"));
            }
            if !self.transition(id, "Downloading", "Committing")? {
                return self.job(id);
            }
            self.commit_file(&job)?;
            return self.job(id);
        }
        let free = match fs2::available_space(&self.paths.content) {
            Ok(value) => value,
            Err(error) => {
                self.state(
                    id,
                    "Failed",
                    Some("cannot inspect available content-root space"),
                )?;
                return Err(error.into());
            }
        };
        if free < job.expected_size.saturating_sub(job.progress) {
            self.state(id, "Failed", Some("insufficient content-root space"))?;
            return Err(invalid("insufficient content-root space"));
        }
        let mut retries = 0;
        loop {
            match self.transfer_once(client, id, pause_after, observer, &mut network_bytes) {
                Ok(()) => return self.job(id),
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
                Err(error) => {
                    if matches!(self.job(id)?.state.as_str(), "Downloading" | "Verifying") {
                        self.state(id, "Failed", Some(&error.to_string()))?;
                    }
                    return Err(error);
                }
            }
        }
    }
    pub fn reconcile_file_jobs(&mut self) -> Result<(), LibraryError> {
        let ids:Vec<String>=self.conn.prepare("SELECT id FROM download_jobs WHERE artifact_id LIKE 'adfile:sha256:%' AND state IN ('Downloading','Verifying','Committing')")?.query_map([],|r|r.get(0))?.collect::<Result<_,_>>()?;
        for id in ids {
            let _lock = match self.lock_job(&id) {
                Ok(value) => value,
                Err(LibraryError::Invalid(message))
                    if message == "download job is active in another process" =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let job = self.job(&id)?;
            if job.state == "Committing" {
                if self.commit_file(&job).is_err() {
                    self.state(&id, "Failed", Some("commit recovery needs inspection"))?;
                }
            } else {
                self.state(
                    &id,
                    "Paused",
                    Some("interrupted; resume to validate staging"),
                )?;
                self.conn
                    .execute("UPDATE download_jobs SET auto_resume=1 WHERE id=?1", [&id])?;
            }
        }
        Ok(())
    }
    pub fn verified_files(
        &self,
        profile: Option<&ServerProfileId>,
    ) -> Result<Vec<VerifiedFile>, LibraryError> {
        let (valid, issues) = self.inspect_files(profile)?;
        if let Some(issue) = issues.first() {
            return Err(invalid(format!("{}: {}", issue.artifact_id, issue.reason)));
        }
        Ok(valid)
    }

    pub fn inspect_files(
        &self,
        profile: Option<&ServerProfileId>,
    ) -> Result<(Vec<VerifiedFile>, Vec<crate::library::LocalIssue>), LibraryError> {
        let mut stmt=self.conn.prepare("SELECT profile_id,artifact_id,association_id,local_ref,verified_at FROM verified_content WHERE artifact_id LIKE 'adfile:sha256:%' AND (?1 IS NULL OR profile_id=?1) ORDER BY verified_at")?;
        let rows = stmt
            .query_map([profile.map(|p| p.0.as_str())], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut valid = Vec::new();
        let mut issues = Vec::new();
        for (profile_id, artifact_id, association_id, relative, verified_at) in rows {
            let result = (|| -> Result<VerifiedFile, LibraryError> {
                let association = association_id
                    .clone()
                    .ok_or_else(|| invalid("verified file lacks association identity"))?;
                Uuid::parse_str(&profile_id).map_err(|_| invalid("invalid verified profile ID"))?;
                Uuid::parse_str(&association)
                    .map_err(|_| invalid("invalid verified association ID"))?;
                let expected_relative = PathBuf::from("artifacts")
                    .join(&profile_id)
                    .join(&association)
                    .join(digest(&artifact_id)?);
                if Path::new(&relative) != expected_relative {
                    return Err(invalid("unsafe verified content path"));
                }
                let path = self.paths.content.join(&expected_relative);
                existing_dir(&self.paths.content.join("artifacts"))?;
                existing_dir(&self.paths.content.join("artifacts").join(&profile_id))?;
                existing_dir(
                    &self
                        .paths
                        .content
                        .join("artifacts")
                        .join(&profile_id)
                        .join(&association),
                )?;
                let expected = digest(&artifact_id)?;
                let size:i64=self.conn.query_row("SELECT json_extract(metadata_json,'$.size') FROM artifacts WHERE profile_id=?1 AND id=?2",params![profile_id,artifact_id],|r|r.get(0))?;
                if size < 0 || hash_file(&path, size as u64)? != expected {
                    return Err(invalid("verified content missing or modified"));
                }
                Ok(VerifiedFile {
                    profile_id: ServerProfileId(profile_id.clone()),
                    association_id: association,
                    artifact_id: artifact_id.clone(),
                    path,
                    size: size as u64,
                    verified_at,
                })
            })();
            match result {
                Ok(item) => valid.push(item),
                Err(error) => issues.push(crate::library::LocalIssue {
                    profile_id: ServerProfileId(profile_id),
                    association_id,
                    artifact_id,
                    kind: "File".into(),
                    reason: error.to_string(),
                }),
            }
        }
        Ok((valid, issues))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    const ASSOCIATION: &str = "4e2608c2-6d9d-43c5-a263-92ce727eb63d";
    struct Fixture {
        _root: tempfile::TempDir,
        library: Library,
        profile: ServerProfileId,
        client: ApiClient,
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    fn fixture(data: Vec<u8>, mode: &'static str) -> Fixture {
        let hash = format!("{:x}", Sha256::digest(&data));
        let id = format!("adfile:sha256:{hash}");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_in = hits.clone();
        let stop_in = stop.clone();
        let id_in = id.clone();
        let handle = thread::spawn(move || {
            while !stop_in.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut lines = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                lines.read_line(&mut first).unwrap();
                let mut headers = std::collections::HashMap::new();
                loop {
                    let mut line = String::new();
                    lines.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("");
                let (status, ctype, body, extra): (&str, &str, Vec<u8>, Vec<(String, String)>) =
                    if path.starts_with("/api/v1/capabilities") {
                        ("200 OK","application/json",br#"{"apiVersion":"1.0.0","ingestionModes":[],"zipMethods":[],"treeIdentity":"adtree:v1:sha256","fileIdentity":"adfile:sha256","uploadRanges":true,"downloadRanges":true,"limits":{"maxPageSize":100,"maxConcurrentRanges":8,"maxUploadBytes":10000000},"features":[]}"#.to_vec(),vec![])
                    } else if path.starts_with("/api/v1/releases/release/artifacts") {
                        ("200 OK","application/json",format!(r#"{{"items":[{{"id":"{ASSOCIATION}","releaseId":"release","artifactId":"{id_in}"}}],"nextCursor":null}}"#).into_bytes(),vec![])
                    } else if path == "/api/v1/releases/release" {
                        (
                            "200 OK",
                            "application/json",
                            br#"{"id":"release","appId":"app","state":"Published","version":"1"}"#
                                .to_vec(),
                            vec![],
                        )
                    } else if path == "/api/v1/apps/app" {
                        ("200 OK","application/json",br#"{"id":"app","name":"Test","slug":"test","ownerId":"owner","visibility":"Public","state":"Published"}"#.to_vec(),vec![])
                    } else if path == format!("/api/v1/artifacts/{id_in}") {
                        (
                            "200 OK",
                            "application/json",
                            format!(r#"{{"id":"{id_in}","kind":"File","size":{}}}"#, data.len())
                                .into_bytes(),
                            vec![],
                        )
                    } else if path == format!("/api/v1/artifacts/{id_in}/content") {
                        let hit = hits_in.fetch_add(1, Ordering::Relaxed);
                        let range = headers
                            .get("range")
                            .and_then(|v| v.strip_prefix("bytes="))
                            .and_then(|v| v.strip_suffix('-'))
                            .and_then(|v| v.parse::<usize>().ok());
                        let etag = format!("\"adfile-{hash}\"");
                        if mode == "retry503" && hit == 0 {
                            ("503 Service Unavailable","application/problem+json",br#"{"type":"urn:app-depot:problem:retryable_failure","title":"Unavailable","status":503,"code":"retryable_failure"}"#.to_vec(),vec![])
                        } else if mode == "auth" {
                            ("401 Unauthorized","application/problem+json",br#"{"type":"urn:app-depot:problem:unauthorized","title":"Unauthorized","status":401,"code":"unauthorized"}"#.to_vec(),vec![])
                        } else if mode == "range416" && range.is_some() {
                            (
                                "416 Range Not Satisfiable",
                                "application/problem+json",
                                b"{}".to_vec(),
                                vec![("Content-Range".into(), format!("bytes */{}", data.len()))],
                            )
                        } else if let Some(start) = range.filter(|_| mode != "fallback") {
                            let mut bytes = data[start..].to_vec();
                            if mode == "bad_hash" && !bytes.is_empty() {
                                bytes[0] ^= 1;
                            }
                            let cr = if mode == "bad_range" {
                                format!("bytes 0-{}/{}", data.len() - 1, data.len())
                            } else {
                                format!("bytes {start}-{}/{}", data.len() - 1, data.len())
                            };
                            let validator = if mode == "bad_etag" {
                                "\"other\"".to_string()
                            } else {
                                etag
                            };
                            (
                                "206 Partial Content",
                                "application/octet-stream",
                                bytes,
                                vec![("Content-Range".into(), cr), ("ETag".into(), validator)],
                            )
                        } else {
                            let mut bytes = data.clone();
                            if mode == "bad_hash" && !bytes.is_empty() {
                                bytes[0] ^= 1;
                            }
                            let declared = bytes.len();
                            if mode == "truncated" && hit < 4 && !bytes.is_empty() {
                                bytes.truncate(bytes.len() / 2);
                            }
                            let mut extra = if mode == "no_etag" {
                                vec![]
                            } else {
                                vec![("ETag".into(), etag)]
                            };
                            if mode == "truncated" {
                                extra.push(("Content-Length".into(), declared.to_string()));
                            }
                            ("200 OK", "application/octet-stream", bytes, extra)
                        }
                    } else {
                        (
                            "404 Not Found",
                            "application/problem+json",
                            b"{}".to_vec(),
                            vec![],
                        )
                    };
                let mut response =
                    format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\n");
                if !extra.iter().any(|(k, _)| k == "Content-Length") {
                    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
                }
                for (k, v) in extra {
                    response.push_str(&format!("{k}: {v}\r\n"));
                }
                response.push_str("\r\n");
                stream.write_all(response.as_bytes()).unwrap();
                if mode == "slow" {
                    for chunk in body.chunks(64 * 1024) {
                        if stream.write_all(chunk).is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                } else {
                    let _ = stream.write_all(&body);
                }
            }
        });
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::open(crate::library::LibraryPaths {
            metadata: root.path().join("data"),
            content: root.path().join("content"),
        })
        .unwrap();
        let profile = library.add_profile("fixture", &url, false).unwrap().id;
        let client = ApiClient::new(&url, None, false).unwrap();
        Fixture {
            _root: root,
            library,
            profile,
            client,
            hits,
            stop,
            thread: Some(handle),
        }
    }
    fn queue(f: &mut Fixture) -> DownloadJob {
        f.library
            .queue_file(&f.client, &f.profile, "release", ASSOCIATION)
            .unwrap()
    }
    #[test]
    fn queue_file_does_not_advance_catalog_sync_epoch() {
        let mut f = fixture(b"download me".to_vec(), "normal");
        f.library
            .conn
            .execute(
                "UPDATE profiles SET last_sync=42 WHERE id=?1",
                [&f.profile.0],
            )
            .unwrap();
        f.library
            .conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app','{}',42)",
                [&f.profile.0],
            )
            .unwrap();
        queue(&mut f);
        assert_eq!(f.library.profile(&f.profile).unwrap().last_sync, Some(42));
        let seen: i64 = f
            .library
            .conn
            .query_row(
                "SELECT last_seen FROM apps WHERE profile_id=?1 AND id='app'",
                [&f.profile.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(seen, 42);
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn live_observations_count_only_network_bytes_and_not_completion() {
        let data = vec![7u8; 256 * 1024];
        let mut fixture = fixture(data.clone(), "normal");
        let job = queue(&mut fixture);
        let mut first = Vec::new();
        let paused = fixture
            .library
            .run_file_job_observed(&fixture.client, &job.id, Some(1), &mut |event| {
                first.push(event)
            })
            .unwrap();
        assert_eq!(paused.state, "Paused");
        assert!(
            fixture
                .library
                .verified_files(Some(&fixture.profile))
                .unwrap()
                .is_empty()
        );
        let first_bytes = first
            .iter()
            .filter(|event| event.phase == TransferPhase::Downloading)
            .map(|event| event.network_bytes)
            .max()
            .unwrap();
        assert!(first_bytes > 0 && first_bytes < data.len() as u64);
        assert_eq!(first_bytes, paused.progress);
        fixture.library.retry_job(&job.id).unwrap();
        let mut resumed = Vec::new();
        let completed = fixture
            .library
            .run_file_job_observed(&fixture.client, &job.id, None, &mut |event| {
                resumed.push(event)
            })
            .unwrap();
        assert_eq!(completed.state, "Completed");
        let transferred = resumed
            .iter()
            .map(|event| event.network_bytes)
            .max()
            .unwrap();
        assert_eq!(transferred, data.len() as u64 - first_bytes);
        assert!(
            resumed
                .iter()
                .any(|event| event.phase == TransferPhase::WaitingForServer)
        );
        assert!(
            resumed
                .iter()
                .any(|event| event.phase == TransferPhase::Verifying)
        );
        assert!(
            resumed
                .iter()
                .any(|event| event.phase == TransferPhase::SavingVerifiedContent)
        );
    }

    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn zero_and_normal_files_complete_with_offline_materialization() {
        for data in [vec![], b"download me".to_vec()] {
            let mut f = fixture(data.clone(), "normal");
            let job = queue(&mut f);
            assert_eq!(
                f.library
                    .run_file_job(&f.client, &job.id, None)
                    .unwrap()
                    .state,
                "Completed"
            );
            let files = f.library.verified_files(Some(&f.profile)).unwrap();
            assert_eq!(fs::read(&files[0].path).unwrap(), data);
            assert!(
                fs::metadata(&files[0].path)
                    .unwrap()
                    .permissions()
                    .readonly()
            );
            assert_eq!(f.library.verified_files(Some(&f.profile)).unwrap().len(), 1);
        }
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn range_resume_and_full_200_restart() {
        for mode in ["normal", "fallback"] {
            let mut f = fixture(vec![7; CHUNK * 3], mode);
            let job = queue(&mut f);
            assert_eq!(
                f.library
                    .run_file_job(&f.client, &job.id, Some(1))
                    .unwrap()
                    .state,
                "Paused"
            );
            assert!(f.library.job(&job.id).unwrap().progress > 0);
            f.library.retry_job(&job.id).unwrap();
            assert_eq!(
                f.library
                    .run_file_job(&f.client, &job.id, None)
                    .unwrap()
                    .state,
                "Completed"
            );
            assert_eq!(f.hits.load(Ordering::Relaxed), 2);
        }
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn invalid_range_and_changed_etag_are_rejected() {
        for mode in ["bad_range", "bad_etag"] {
            let mut f = fixture(vec![1; CHUNK * 2], mode);
            let job = queue(&mut f);
            f.library.run_file_job(&f.client, &job.id, Some(1)).unwrap();
            f.library.retry_job(&job.id).unwrap();
            assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
            assert_eq!(f.library.job(&job.id).unwrap().state, "Failed");
            assert!(
                f.library
                    .verified_files(Some(&f.profile))
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn range_416_restarts_from_zero_and_verifies() {
        let mut f = fixture(vec![8; CHUNK * 2], "range416");
        let job = queue(&mut f);
        f.library.run_file_job(&f.client, &job.id, Some(1)).unwrap();
        f.library.retry_job(&job.id).unwrap();
        assert_eq!(
            f.library
                .run_file_job(&f.client, &job.id, None)
                .unwrap()
                .state,
            "Completed"
        );
        assert_eq!(f.hits.load(Ordering::Relaxed), 3);
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn temporary_http_failure_is_retried_but_cancellation_is_terminal() {
        let mut f = fixture(vec![10; CHUNK], "retry503");
        let job = queue(&mut f);
        assert_eq!(
            f.library
                .run_file_job(&f.client, &job.id, None)
                .unwrap()
                .state,
            "Completed"
        );
        assert_eq!(f.hits.load(Ordering::Relaxed), 2);
        let mut f = fixture(vec![11; CHUNK], "normal");
        let job = queue(&mut f);
        assert_eq!(f.library.cancel_job(&job.id).unwrap().state, "Cancelled");
        assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
        assert!(
            f.library
                .verified_files(Some(&f.profile))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn corrupted_stage_restarts_without_trusting_checkpoint() {
        let mut f = fixture(vec![9; CHUNK * 2], "normal");
        let job = queue(&mut f);
        f.library.run_file_job(&f.client, &job.id, Some(1)).unwrap();
        let path = f.library.stage(&job.id).unwrap();
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.write_all(b"bad").unwrap();
        file.sync_all().unwrap();
        f.library.retry_job(&job.id).unwrap();
        assert_eq!(
            f.library
                .run_file_job(&f.client, &job.id, None)
                .unwrap()
                .state,
            "Completed"
        );
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn database_failure_during_completion_never_reports_completed() {
        let mut f = fixture(vec![6; CHUNK], "normal");
        let job = queue(&mut f);
        f.library.conn.execute_batch("CREATE TRIGGER reject_complete BEFORE UPDATE ON download_jobs WHEN NEW.state='Completed' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
        assert_ne!(f.library.job(&job.id).unwrap().state, "Completed");
        assert!(
            f.library
                .verified_files(Some(&f.profile))
                .unwrap()
                .is_empty()
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_materialization_is_rejected_without_completion() {
        let mut f = fixture(vec![12; CHUNK], "normal");
        let job = queue(&mut f);
        let parent = f.library.paths.content.join("artifacts").join(&f.profile.0);
        private_dir(&parent).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), parent.join(ASSOCIATION)).unwrap();
        assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
        assert_ne!(f.library.job(&job.id).unwrap().state, "Completed");
        assert!(
            f.library
                .verified_files(Some(&f.profile))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn corruption_truncation_and_authorization_never_complete() {
        for mode in ["bad_hash", "truncated", "auth", "no_etag"] {
            let mut f = fixture(vec![2; CHUNK], mode);
            let job = queue(&mut f);
            assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
            assert_eq!(f.library.job(&job.id).unwrap().state, "Failed");
            assert!(
                f.library
                    .verified_files(Some(&f.profile))
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn interrupted_job_reopens_and_resumes_from_checkpoint() {
        let mut f = fixture(vec![3; CHUNK * 3], "normal");
        let job = queue(&mut f);
        f.library
            .run_file_job(&f.client, &job.id, Some(CHUNK as u64))
            .unwrap();
        f.library
            .conn
            .execute(
                "UPDATE download_jobs SET state='Downloading' WHERE id=?1",
                [&job.id],
            )
            .unwrap();
        let paths = f.library.paths.clone();
        f.library = Library::open(paths).unwrap();
        f.library.reconcile_file_jobs().unwrap();
        assert_eq!(f.library.job(&job.id).unwrap().state, "Paused");
        f.library.retry_job(&job.id).unwrap();
        assert_eq!(
            f.library
                .run_file_job(&f.client, &job.id, None)
                .unwrap()
                .state,
            "Completed"
        );
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn object_reuse_and_profile_context_remain_distinct() {
        let mut f = fixture(vec![4; CHUNK], "normal");
        let one = queue(&mut f);
        f.library.run_file_job(&f.client, &one.id, None).unwrap();
        let hits = f.hits.load(Ordering::Relaxed);
        let second = f
            .library
            .add_profile("another", f.client.origin(), false)
            .unwrap()
            .id;
        let other = f
            .library
            .queue_file(&f.client, &second, "release", ASSOCIATION)
            .unwrap();
        f.library.run_file_job(&f.client, &other.id, None).unwrap();
        assert_eq!(f.hits.load(Ordering::Relaxed), hits);
        let files = f.library.verified_files(None).unwrap();
        assert_eq!(files.len(), 2);
        assert_ne!(files[0].path, files[1].path);
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn crash_during_commit_reconciles_and_lock_prevents_concurrent_run() {
        let mut f = fixture(vec![5; CHUNK], "normal");
        let job = queue(&mut f);
        let lock = f.library.lock_job(&job.id).unwrap();
        assert!(f.library.run_file_job(&f.client, &job.id, None).is_err());
        drop(lock);
        f.library
            .run_file_job(&f.client, &job.id, Some(CHUNK as u64))
            .unwrap();
        f.library
            .conn
            .execute(
                "UPDATE download_jobs SET state='Committing' WHERE id=?1",
                [&job.id],
            )
            .unwrap();
        f.library.reconcile_file_jobs().unwrap();
        assert_eq!(f.library.job(&job.id).unwrap().state, "Completed");
    }
    #[test]
    fn queue_priority_preemption_and_user_pause_are_distinct() {
        let mut f = fixture(vec![1], "normal");
        let first = queue(&mut f);
        for (rank, association) in [(2, "second"), (3, "third")] {
            f.library.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,association_id,state,created_at,updated_at,expected_size,queue_rank) VALUES (?1,?2,?3,?4,'Queued',0,0,1,?5)",params![Uuid::new_v4().to_string(),f.profile.0,first.artifact_id,association,rank]).unwrap();
        }
        let next = f.library.next_queued_job().unwrap().unwrap();
        assert_eq!(next.association_id, "third");
        f.library.move_job_to_top(&first.id).unwrap();
        assert_eq!(f.library.next_queued_job().unwrap().unwrap().id, first.id);
        f.library
            .conn
            .execute(
                "UPDATE download_jobs SET state='Downloading' WHERE id=?1",
                [&first.id],
            )
            .unwrap();
        assert!(f.library.preempt_job(&first.id).unwrap());
        assert!(f.library.job(&first.id).unwrap().auto_resume);
        f.library.restore_preempted_jobs().unwrap();
        assert_eq!(f.library.job(&first.id).unwrap().state, "Queued");
        f.library.pause_job(&first.id).unwrap();
        f.library.restore_preempted_jobs().unwrap();
        assert_eq!(f.library.job(&first.id).unwrap().state, "Paused");
        assert!(!f.library.job(&first.id).unwrap().auto_resume);
        let paths = f.library.paths.clone();
        let reopened = Library::open(paths).unwrap();
        assert_eq!(reopened.job(&first.id).unwrap().state, "Paused");
        assert!(!reopened.job(&first.id).unwrap().auto_resume);
        assert_eq!(
            reopened.next_queued_job().unwrap().unwrap().association_id,
            "third"
        );
    }
    #[test]
    fn repeated_association_request_reuses_pending_job() {
        let mut f = fixture(vec![1], "normal");
        let first = queue(&mut f);
        let repeated = f
            .library
            .queue_file(&f.client, &f.profile, "release", ASSOCIATION)
            .unwrap();
        assert_eq!(first.id, repeated.id);
        assert_eq!(f.library.jobs(Some(&f.profile)).unwrap().len(), 1);
    }
    #[test]
    #[cfg_attr(windows, ignore = "requires durable File staging and publication")]
    fn preemption_keeps_durable_checkpoint_and_resumes_without_discarding_bytes() {
        let mut f = fixture(vec![7; 5 * 1024 * 1024], "slow");
        let job = queue(&mut f);
        let paths = f.library.paths.clone();
        let origin = f.client.origin().to_owned();
        let id = job.id.clone();
        let worker = thread::spawn(move || {
            let mut lib = Library::open(paths).unwrap();
            let api = ApiClient::new(&origin, None, false).unwrap();
            lib.run_file_job(&api, &id, None).unwrap()
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while f.hits.load(Ordering::Relaxed) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "download did not start"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(f.library.lock_download_slot().is_err());
        assert!(f.library.preempt_job(&job.id).unwrap());
        f.library.restore_preempted_jobs().unwrap();
        assert_eq!(f.library.job(&job.id).unwrap().state, "Paused");
        assert_eq!(worker.join().unwrap().state, "Paused");
        let paused = f.library.job(&job.id).unwrap();
        assert!(paused.progress >= 4 * 1024 * 1024 && paused.progress < paused.expected_size);
        f.library.restore_preempted_jobs().unwrap();
        assert_eq!(f.library.job(&job.id).unwrap().state, "Queued");
        assert_eq!(
            f.library
                .run_file_job(&f.client, &job.id, None)
                .unwrap()
                .state,
            "Completed"
        );
    }

    #[test]
    fn separate_connections_wait_for_short_sqlite_writer() {
        let f = fixture(vec![1], "normal");
        let paths = f.library.paths.clone();
        let id = f.profile.0.clone();
        f.library.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let worker = std::thread::spawn(move || {
            let lib = Library::open(paths).unwrap();
            lib.conn
                .execute("UPDATE profiles SET name='second' WHERE id=?1", [id])
                .unwrap();
        });
        std::thread::sleep(Duration::from_millis(75));
        f.library.conn.execute_batch("COMMIT").unwrap();
        worker.join().unwrap();
        assert_eq!(f.library.profile(&f.profile).unwrap().name, "second");
    }
    #[test]
    fn migration_from_v1_preserves_existing_profile() {
        let f = fixture(vec![1], "normal");
        let paths = f.library.paths.clone();
        let id = f.profile.clone();
        f.library.conn.execute_batch("PRAGMA user_version=1; DROP TABLE installations; DROP TABLE tree_files; DROP INDEX download_jobs_active; ALTER TABLE download_jobs DROP COLUMN auto_resume; ALTER TABLE download_jobs DROP COLUMN queue_rank; ALTER TABLE download_jobs DROP COLUMN inventory_json; ALTER TABLE download_jobs DROP COLUMN inventory_etag; ALTER TABLE download_jobs DROP COLUMN association_id; ALTER TABLE download_jobs DROP COLUMN expected_size; ALTER TABLE download_jobs DROP COLUMN etag; ALTER TABLE download_jobs DROP COLUMN progress; ALTER TABLE download_jobs DROP COLUMN checkpoint_hash; ALTER TABLE download_jobs DROP COLUMN last_error; ALTER TABLE download_jobs DROP COLUMN updated_at; ALTER TABLE download_jobs DROP COLUMN attempts; ALTER TABLE verified_content DROP COLUMN association_id;").unwrap();
        let library = Library::open(paths).unwrap();
        assert_eq!(library.profile(&id).unwrap().name, "fixture");
    }
    #[test]
    fn migration_from_v2_preserves_file_job() {
        let mut f = fixture(vec![1], "normal");
        let job = queue(&mut f);
        let paths = f.library.paths.clone();
        f.library.conn.execute_batch("PRAGMA user_version=2; DROP TABLE installations; DROP TABLE tree_files; DROP INDEX download_jobs_active; ALTER TABLE download_jobs DROP COLUMN auto_resume; ALTER TABLE download_jobs DROP COLUMN queue_rank; ALTER TABLE download_jobs DROP COLUMN inventory_json; ALTER TABLE download_jobs DROP COLUMN inventory_etag; CREATE UNIQUE INDEX download_jobs_active ON download_jobs(profile_id,artifact_id,association_id) WHERE state IN ('Queued','Downloading','Verifying','Committing','Paused');").unwrap();
        let library = Library::open(paths).unwrap();
        assert_eq!(library.job(&job.id).unwrap().artifact_id, job.artifact_id);
        assert_eq!(library.job(&job.id).unwrap().state, "Queued");
    }
}
