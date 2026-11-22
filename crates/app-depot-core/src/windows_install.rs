//! Windows managed Artifact installation using the shared installation journal.
use crate::{
    library::{Library, LibraryError, now},
    managed::Installation,
    models::InventoryEntry,
    transfer::{DownloadJob, hash_file, hash_open_file},
    windows_fs,
};
use rusqlite::{OptionalExtension, params};
use std::{fs::File, io};
use uuid::Uuid;

fn invalid(message: &str) -> LibraryError {
    LibraryError::Invalid(message.into())
}

fn verify(file: &File, size: u64, digest: &str) -> io::Result<()> {
    if hash_open_file(file, size).map_err(io::Error::other)? != digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "installed payload SHA-256 mismatch",
        ));
    }
    Ok(())
}

impl Library {
    /// Explicitly install a completed File job. This operation does not interpret launch declarations.
    pub fn install_windows_file(&mut self, job_id: &str) -> Result<Installation, LibraryError> {
        if !self.job(job_id)?.artifact_id.starts_with("adfile:sha256:") {
            return Err(invalid("expected File Artifact"));
        }
        self.install_windows_artifact(job_id)
    }

    pub fn install_windows_artifact(&mut self, job_id: &str) -> Result<Installation, LibraryError> {
        let _slot = self.lock_download_slot()?;
        let job = self.job(job_id)?;
        let payload = self.verified_windows_payload(&job)?;
        let existing =
            self.installation_for(&job.profile_id, &job.association_id, &job.artifact_id)?;
        let id = match existing {
            Some(item) if item.state == "Ready" => {
                self.verify_windows_installation(&item.id)?;
                return Ok(item);
            }
            Some(item) if item.state == "Preparing" => item.id,
            Some(_) => return Err(invalid("installation must be inspected before retry")),
            None => {
                let (app, release): (String, String) = self.conn.query_row(
                    "SELECT r.app_id,r.id FROM associations a JOIN releases r ON r.profile_id=a.profile_id AND r.id=a.release_id WHERE a.profile_id=?1 AND a.id=?2 AND a.artifact_id=?3",
                    params![job.profile_id.0, job.association_id, job.artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let id = Uuid::new_v4().to_string();
                self.conn.execute(
                    "INSERT INTO installations(id,profile_id,app_id,release_id,association_id,artifact_id,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,'Preparing',?7,?7)",
                    params![id, job.profile_id.0, app, release, job.association_id, job.artifact_id, now()?],
                )?;
                id
            }
        };
        #[cfg(test)]
        crate::transfer::pause_file_commit_test(&self.stage(job_id)?, "after-preparing");
        let item = self.installation(&id)?;
        match payload.verify_installation(&item, &job) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => match payload {
                WindowsPayload::File(digest) => {
                    let object = self.object(&job.artifact_id)?;
                    windows_fs::publish_file_install(
                        &object,
                        &item.path,
                        &id,
                        job.expected_size,
                        |file| verify(file, job.expected_size, digest),
                    )?;
                }
                WindowsPayload::Tree(entries) => {
                    windows_fs::publish_tree_install(&item.path, &id, &entries, |digest| {
                        self.object(&format!("adfile:sha256:{digest}"))
                            .map_err(io::Error::other)
                    })?;
                }
            },
            Err(error) => return Err(error.into()),
        }
        #[cfg(test)]
        crate::transfer::pause_file_commit_test(&self.stage(job_id)?, "after-install-publication");
        self.activate_windows_installation(&item, &job)?;
        #[cfg(test)]
        crate::transfer::pause_file_commit_test(&self.stage(job_id)?, "after-activation");
        self.installation(&id)
    }

    /// Activate a previously published, verified File install. An absent directory is left for
    /// explicit retry. Existing unexpected data is preserved rather than treated as staging.
    pub fn reconcile_windows_installations(&mut self) -> Result<(), LibraryError> {
        let _slot = self.lock_download_slot()?;
        for item in self.installations(None)? {
            if item.state != "Preparing" {
                continue;
            }
            if let Err(error) = self
                .completed_install_job(&item)
                .and_then(|job| self.activate_windows_installation(&item, &job))
            {
                if matches!(&error, LibraryError::Io(error) if error.kind() == io::ErrorKind::NotFound)
                {
                    continue;
                }
                self.conn.execute(
                    "UPDATE installations SET last_error=?2 WHERE id=?1 AND state='Preparing'",
                    params![item.id, error.to_string()],
                )?;
            }
        }
        Ok(())
    }

    pub fn verify_windows_installation(&self, id: &str) -> Result<(), LibraryError> {
        let item = self.installation(id)?;
        if item.state != "Ready" {
            return Err(invalid("installation is not ready"));
        }
        let job = self.completed_install_job(&item)?;
        let payload = self.verified_windows_payload(&job)?;
        payload.verify_installation(&item, &job)?;
        Ok(())
    }

    fn verified_windows_payload<'a>(
        &self,
        job: &'a DownloadJob,
    ) -> Result<WindowsPayload<'a>, LibraryError> {
        let registered: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM verified_content WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3)",
            params![job.profile_id.0, job.association_id, job.artifact_id],
            |row| row.get(0),
        )?;
        if job.state != "Completed" || !registered {
            return Err(invalid(
                "a completed verified Artifact download is required",
            ));
        }
        if let Some(digest) = job.artifact_id.strip_prefix("adfile:sha256:") {
            let object = self.object(&job.artifact_id)?;
            if hash_file(&object, job.expected_size)? != digest
                || hash_file(&self.materialized(job)?, job.expected_size)? != digest
            {
                return Err(invalid("verified File bytes are missing or modified"));
            }
            return Ok(WindowsPayload::File(digest));
        }
        let entries = self
            .tree_inventory(&job.id)?
            .ok_or_else(|| invalid("Tree inventory missing"))?;
        if crate::tree::identity(&entries)? != job.artifact_id {
            return Err(invalid("Tree identity mismatch"));
        }
        crate::tree::validate_destination(&entries)?;
        let visible = crate::tree_download::tree_destination(self, job)?;
        self.verify_tree_directory(&visible, &entries)
            .map_err(|error| invalid(&format!("verified Tree download is unavailable: {error}")))?;
        Ok(WindowsPayload::Tree(entries))
    }

    fn completed_install_job(&self, item: &Installation) -> Result<DownloadJob, LibraryError> {
        let id: String = self.conn.query_row(
            "SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Completed' ORDER BY updated_at DESC LIMIT 1",
            params![item.profile_id.0, item.association_id, item.artifact_id],
            |row| row.get(0),
        ).optional()?.ok_or_else(|| invalid("installation lacks a completed download job"))?;
        self.job(&id)
    }

    fn activate_windows_installation(
        &mut self,
        item: &Installation,
        job: &DownloadJob,
    ) -> Result<(), LibraryError> {
        self.verified_windows_payload(job)?
            .verify_installation(item, job)?;
        windows_fs::sync_directory(item.path.parent().unwrap())?;
        #[cfg(test)]
        windows_fs::pause_tree_test("before-tree-ready");
        if self.conn.execute(
            "UPDATE installations SET state='Ready',updated_at=?2,last_error=NULL WHERE id=?1 AND state='Preparing'",
            params![item.id, now()?],
        )? != 1 {
            return Err(invalid("installation left Preparing before activation"));
        }
        Ok(())
    }
}

enum WindowsPayload<'a> {
    File(&'a str),
    Tree(Vec<InventoryEntry>),
}

impl WindowsPayload<'_> {
    fn verify_installation(&self, item: &Installation, job: &DownloadJob) -> io::Result<()> {
        match self {
            Self::File(digest) => verify(
                &windows_fs::inspect_installed_payload(&item.path, &item.id)?,
                job.expected_size,
                digest,
            ),
            Self::Tree(entries) => windows_fs::verify_tree_install(&item.path, &item.id, entries),
        }
    }
}
