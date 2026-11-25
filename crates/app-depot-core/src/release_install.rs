//! Release-scoped transactional installation generations.
#[cfg(any(target_os = "linux", windows))]
use crate::installation::{Plan, ResolveRequest};
use crate::{
    ServerProfileId,
    library::{Library, LibraryError},
};
#[cfg(any(target_os = "linux", windows))]
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;
use std::path::PathBuf;

#[cfg(any(target_os = "linux", windows))]
fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    pub id: String,
    pub plan_id: String,
    pub state: String,
    pub last_error: Option<String>,
    pub path: PathBuf,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInstallation {
    pub id: String,
    pub profile_id: ServerProfileId,
    pub app_id: String,
    pub release_id: String,
    pub active_generation: Option<String>,
    pub generations: Vec<Generation>,
}
impl Library {
    #[cfg(any(target_os = "linux", windows))]
    fn load_install_plan(&self, profile: &ServerProfileId, id: &str) -> Result<Plan, LibraryError> {
        let saved = self
            .saved_plans(profile)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| invalid("saved plan not found"))?;
        let parsed: Plan = serde_json::from_value(saved.plan.clone())?;
        let request = ResolveRequest {
            platform: parsed.platform.clone(),
            revision_id: Some(parsed.revision_id.clone()),
            selections: parsed.selections.clone(),
        };
        let plan = Plan::parse(&saved.plan, &parsed.release_id, &request)?;
        if plan.recipe.is_some() {
            return Err(invalid("Derived installation is unsupported"));
        }
        Ok(plan)
    }

    pub fn release_installation(
        &self,
        profile: &ServerProfileId,
        release: &str,
    ) -> Result<Option<ReleaseInstallation>, LibraryError> {
        // One query gives status a consistent SQLite snapshot across activation.
        let mut stmt=self.conn.prepare("SELECT i.id,i.app_id,i.active_generation,g.id,g.plan_id,g.state,g.last_error FROM release_installations i LEFT JOIN installation_generations g ON g.installation_id=i.id WHERE i.profile_id=?1 AND i.release_id=?2 ORDER BY g.rowid")?;
        let mut rows = stmt.query(params![profile.0, release])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let mut installation = ReleaseInstallation {
            id: first.get(0)?,
            app_id: first.get(1)?,
            active_generation: first.get(2)?,
            profile_id: profile.clone(),
            release_id: release.into(),
            generations: Vec::new(),
        };
        let mut current = Some(first);
        while let Some(row) = current {
            if let Some(id) = row.get::<_, Option<String>>(3)? {
                installation.generations.push(Generation {
                    path: self.paths.content.join("apps").join(&id).join("payload"),
                    id,
                    plan_id: row.get(4)?,
                    state: row.get(5)?,
                    last_error: row.get(6)?,
                });
            }
            current = rows.next()?;
        }
        Ok(Some(installation))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn source_in_use(
        &self,
        profile: &ServerProfileId,
        association: &str,
        artifact: &str,
    ) -> Result<bool, LibraryError> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM installation_sources s JOIN installation_generations g ON g.id=s.generation_id JOIN release_installations i ON i.id=g.installation_id WHERE i.profile_id=?1 AND s.association_id=?2 AND s.artifact_id=?3)",params![profile.0,association,artifact],|r|r.get(0))?)
    }
}

#[cfg(any(target_os = "linux", windows))]
mod coordinator {
    use super::*;
    use crate::{
        api::ApiClient,
        installation::Source,
        materialization::{destination_layout, publish_generation, verify_generation},
        models::InventoryEntry,
        transfer::DownloadJob,
    };
    #[cfg(target_os = "linux")]
    use crate::{
        launch::LaunchCandidate,
        managed::{delete_owned, process_start, runtime_busy},
    };
    #[cfg(target_os = "linux")]
    use std::process::Child;
    use std::{collections::BTreeMap, fs};
    use uuid::Uuid;

    fn canonical(id: &str) -> Result<(), LibraryError> {
        if Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id) {
            Ok(())
        } else {
            Err(invalid("invalid local generation identity"))
        }
    }
    impl Library {
        #[cfg(target_os = "linux")]
        fn manifest_lifecycle_lock(&self) -> Result<std::fs::File, LibraryError> {
            self.lifecycle_lock()
        }
        #[cfg(windows)]
        fn manifest_lifecycle_lock(&self) -> Result<std::fs::File, LibraryError> {
            use crate::transfer::{create_content_file, open_regular};
            use fs2::FileExt;
            let path = self.paths.metadata.join(".app-depot-manifest.lock");
            let file = match create_content_file(&path) {
                Ok(f) => f,
                Err(LibraryError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_regular(&path, true)?
                }
                Err(e) => return Err(e),
            };
            file.try_lock_exclusive()
                .map_err(|_| invalid("another Standard installation operation is active"))?;
            Ok(file)
        }
        /// Explicit install/retry. No prerequisite execution or launch occurs here.
        pub fn install_saved_plan(
            &mut self,
            profile: &ServerProfileId,
            plan_id: &str,
            client: Option<&ApiClient>,
        ) -> Result<ReleaseInstallation, LibraryError> {
            let _lifecycle = self.manifest_lifecycle_lock()?;
            let plan = self.load_install_plan(profile, plan_id)?;
            if crate::launch::host() != Some(&plan.platform) {
                return Err(invalid("installation platform does not match this host"));
            }
            let layout = destination_layout(&plan)?;
            let files = plan.files.as_ref().expect("Standard plan");
            let mut sources: BTreeMap<(&str, &str), Vec<&Source>> = BTreeMap::new();
            for source in files.values() {
                sources
                    .entry((&source.association_id, &source.artifact_id))
                    .or_default()
                    .push(source);
            }
            for (association, artifact) in sources.keys() {
                if self.conn.query_row("SELECT EXISTS(SELECT 1 FROM installations WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Removing' AND remove_complete=1)",params![profile.0,association,artifact],|r| r.get::<_,bool>(0))? {return Err(invalid("source removal is pending; complete removal before installing"));}
            }
            let existing = self.release_installation(profile, &plan.release_id)?;
            if existing
                .as_ref()
                .is_some_and(|i| i.generations.iter().any(|g| g.state == "Removing"))
            {
                return Err(invalid("finish interrupted uninstall first"));
            }
            let install_id = existing
                .as_ref()
                .map_or_else(|| Uuid::new_v4().to_string(), |i| i.id.clone());
            let pending = existing
                .as_ref()
                .and_then(|i| i.generations.iter().find(|g| g.state == "Preparing"));
            if pending.is_some_and(|g| g.plan_id != plan_id) {
                return Err(invalid(
                    "retry the pending plan or uninstall before selecting another plan",
                ));
            }
            let id = pending.map_or_else(|| Uuid::new_v4().to_string(), |g| g.id.clone());
            canonical(&id)?;
            let tx = self.conn.transaction()?;
            tx.execute("INSERT OR IGNORE INTO release_installations(id,profile_id,app_id,release_id) VALUES (?1,?2,?3,?4)",params![install_id,profile.0,plan.app_id,plan.release_id])?;
            if pending.is_none() {
                tx.execute("INSERT INTO installation_generations(id,installation_id,plan_id,state) VALUES (?1,?2,?3,'Preparing')",params![id,install_id,plan_id])?;
                for (association, artifact) in sources.keys() {
                    tx.execute("INSERT INTO installation_sources(generation_id,association_id,artifact_id) VALUES (?1,?2,?3)",params![id,association,artifact])?;
                }
            }
            tx.commit()?;
            let prepared = (|| -> Result<(), LibraryError> {
                for ((association, artifact), files) in &sources {
                    let job = self.acquire_plan_source(
                        profile,
                        &plan.release_id,
                        association,
                        artifact,
                        client,
                    )?;
                    self.authorize_plan_files(&job, files)?;
                }
                let _download = self.lock_download_slot()?;
                let target = self.paths.content.join("apps").join(&id);
                match fs::symlink_metadata(&target) {
                    Ok(_) => verify_generation(&target, &id, &layout)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        publish_generation(&self.paths.content, &id, &layout, |digest| {
                            self.object(&format!("adfile:sha256:{digest}"))
                        })?
                    }
                    Err(e) => return Err(e.into()),
                }
                crate::transfer::sync_dir(&self.paths.content.join("apps"))?;
                crate::transfer::sync_dir(&self.paths.content.join("staging"))?;
                let state = if plan
                    .prerequisites
                    .as_ref()
                    .expect("Standard plan")
                    .iter()
                    .any(|p| !p.optional)
                {
                    "PrerequisitesPending"
                } else {
                    "Ready"
                };
                let tx = self.conn.transaction()?;
                tx.execute("UPDATE installation_generations SET state='Retired' WHERE id=(SELECT active_generation FROM release_installations WHERE id=?1)",[&install_id])?;
                if tx.execute("UPDATE installation_generations SET state=?2,last_error=NULL WHERE id=?1 AND installation_id=?3 AND state='Preparing'",params![id,state,install_id])? != 1 {return Err(invalid("pending generation changed before activation"));}
                tx.execute(
                    "UPDATE release_installations SET active_generation=?2 WHERE id=?1",
                    params![install_id, id],
                )?;
                tx.commit()?;
                Ok(())
            })();
            if let Err(e) = prepared {
                self.conn.execute(
                    "UPDATE installation_generations SET last_error=?2 WHERE id=?1",
                    params![id, e.to_string()],
                )?;
                return Err(e);
            }
            self.release_installation(profile, &plan.release_id)?
                .ok_or_else(|| invalid("installation missing after activation"))
        }

        fn acquire_plan_source(
            &mut self,
            profile: &ServerProfileId,
            release: &str,
            association: &str,
            artifact: &str,
            client: Option<&ApiClient>,
        ) -> Result<DownloadJob, LibraryError> {
            let completed:Option<String>=self.conn.query_row("SELECT j.id FROM download_jobs j JOIN verified_content v ON (j.profile_id,j.association_id,j.artifact_id)=(v.profile_id,v.association_id,v.artifact_id) WHERE j.profile_id=?1 AND j.association_id=?2 AND j.artifact_id=?3 AND j.state='Completed' ORDER BY j.updated_at DESC LIMIT 1",params![profile.0,association,artifact],|r|r.get(0)).optional()?;
            if let Some(id) = completed {
                return self.job(&id);
            }
            let client = client.ok_or_else(|| {
                invalid("source requires a download; connect the selected profile")
            })?;
            self.reconcile_file_jobs()?;
            self.reconcile_tree_jobs()?;
            let mut job = if artifact.starts_with("adfile:") {
                self.queue_file(client, profile, release, association)?
            } else {
                self.queue_tree(client, profile, release, association)?
            };
            if job.artifact_id != artifact {
                return Err(invalid(
                    "Association no longer references the saved Artifact",
                ));
            }
            if matches!(job.state.as_str(), "Paused" | "Failed") {
                job = self.retry_job(&job.id)?;
            }
            let job = if artifact.starts_with("adfile:") {
                self.run_file_job(client, &job.id, None)?
            } else {
                self.run_tree_job(client, &job.id, None)?
            };
            if job.state != "Completed" {
                return Err(invalid("download did not complete"));
            }
            Ok(job)
        }
        fn authorize_plan_files(
            &self,
            job: &DownloadJob,
            files: &[&Source],
        ) -> Result<(), LibraryError> {
            let inventory = if job.artifact_id.starts_with("adtree:") {
                let entries = self
                    .tree_inventory(&job.id)?
                    .ok_or_else(|| invalid("missing verified Tree inventory"))?;
                if crate::tree::identity(&entries)? != job.artifact_id {
                    return Err(invalid("Tree identity mismatch"));
                }
                Some(
                    entries
                        .into_iter()
                        .filter_map(|entry| match entry {
                            InventoryEntry::File {
                                file_id,
                                size,
                                sha256,
                                executable,
                                ..
                            } => Some((file_id, (size, sha256, executable))),
                            _ => None,
                        })
                        .collect::<BTreeMap<_, _>>(),
                )
            } else {
                None
            };
            let mut checked = std::collections::HashSet::new();
            for source in files {
                if let Some(entries) = &inventory {
                    if source
                        .file_id
                        .as_ref()
                        .and_then(|id| entries.get(id))
                        .is_none_or(|(size, hash, executable)| {
                            *size != source.size
                                || *hash != source.sha256
                                || *executable != source.executable
                        })
                    {
                        return Err(invalid("saved source does not match verified Tree member"));
                    }
                } else if source.file_id.is_some()
                    || source.size != job.expected_size
                    || job.artifact_id != format!("adfile:sha256:{}", source.sha256)
                {
                    return Err(invalid("saved source does not match verified File"));
                }
                if checked.insert((&source.sha256, source.size))
                    && crate::transfer::hash_file(
                        &self.object(&format!("adfile:sha256:{}", source.sha256))?,
                        source.size,
                    )? != source.sha256
                {
                    return Err(invalid("source CAS digest mismatch"));
                }
            }
            Ok(())
        }

        pub fn verify_release_installation(
            &self,
            profile: &ServerProfileId,
            release: &str,
        ) -> Result<(), LibraryError> {
            let _lock = self.manifest_lifecycle_lock()?;
            let g = self.active_release_generation(profile, release)?;
            let plan = self.load_install_plan(profile, &g.plan_id)?;
            verify_generation(
                &self.paths.content.join("apps").join(&g.id),
                &g.id,
                &destination_layout(&plan)?,
            )
        }
        fn active_release_generation(
            &self,
            profile: &ServerProfileId,
            release: &str,
        ) -> Result<Generation, LibraryError> {
            let i = self
                .release_installation(profile, release)?
                .ok_or_else(|| invalid("Release installation not found"))?;
            let g = i
                .generations
                .into_iter()
                .find(|g| Some(&g.id) == i.active_generation.as_ref())
                .ok_or_else(|| invalid("no active generation"))?;
            canonical(&g.id)?;
            if !matches!(g.state.as_str(), "Ready" | "PrerequisitesPending") {
                return Err(invalid("active generation is unavailable"));
            }
            Ok(g)
        }
        #[cfg(target_os = "linux")]
        pub(crate) fn manifest_launch_candidate(
            &self,
            profile: &ServerProfileId,
            release: &str,
        ) -> Result<Option<LaunchCandidate>, LibraryError> {
            let Some(i) = self.release_installation(profile, release)? else {
                return Ok(None);
            };
            if i.active_generation.is_none() {
                return Ok(None);
            }
            let g = self.active_release_generation(profile, release)?;
            let plan = self.load_install_plan(profile, &g.plan_id)?;
            let Some(launch) = plan.launch else {
                return Ok(None);
            };
            let source = &plan.files.as_ref().expect("Standard plan")[&launch.path];
            let ready = g.state == "Ready" && plan.platform == crate::launch::host().unwrap_or("");
            let mut candidate = LaunchCandidate {
                profile_id: profile.clone(),
                release_id: release.into(),
                association_id: launch.association_id,
                artifact_id: source.artifact_id.clone(),
                entry_id: launch.entry_point_id,
                declaration_revision: launch.revision_id,
                generation_id: Some(g.id.clone()),
                label: None,
                target: Some(launch.path),
                working_directory: Some(launch.working_directory),
                arguments: launch.arguments,
                status: if ready { "Ready" } else { "Unavailable" }.into(),
                reason: if ready {
                    "Installed Standard plan"
                } else {
                    "Required prerequisites are incomplete or platform mismatches"
                }
                .into(),
                local_root: Some(g.path),
            };
            if ready
                && let Err(reason) = crate::materialization::verify_linux_marker(
                    &self.paths.content.join("apps").join(&g.id),
                    &g.id,
                )
                .map_err(|e| e.to_string())
                .and_then(|_| candidate.local_readiness())
            {
                candidate.status = "Unavailable".into();
                candidate.reason = reason;
            }
            Ok(Some(candidate))
        }
        #[cfg(target_os = "linux")]
        pub(crate) fn play_manifest(
            &self,
            profile: &ServerProfileId,
            release: &str,
            association: &str,
            entry: &str,
            revision: &str,
        ) -> Result<Child, LibraryError> {
            let g = self.active_release_generation(profile, release)?;
            let c = self
                .manifest_launch_candidate(profile, release)?
                .ok_or_else(|| invalid("saved plan has no launch"))?;
            if c.association_id != association
                || c.entry_id != entry
                || c.selection_revision() != revision
            {
                return Err(invalid("installed launch choice changed"));
            }
            let tracked: (Option<i64>, Option<String>) = self.conn.query_row(
                "SELECT process_id,process_start FROM installation_generations WHERE id=?1",
                [&g.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if runtime_busy(&g.path, tracked.0, tracked.1.as_deref())? {
                return Err(invalid("installation is running"));
            }
            let mut child = c.spawn()?;
            let record = (|| -> Result<(), LibraryError> {
                let start = process_start(child.id())?.unwrap_or_default();
                self.conn.execute("UPDATE installation_generations SET process_id=?2,process_start=?3 WHERE id=?1",params![g.id,i64::from(child.id()),start])?;
                Ok(())
            })();
            if let Err(e) = record {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
            Ok(child)
        }
        /// Explicit runtime-only uninstall of every generation; source downloads remain.
        #[cfg(target_os = "linux")]
        pub fn uninstall_release_installation(
            &mut self,
            profile: &ServerProfileId,
            release: &str,
        ) -> Result<(), LibraryError> {
            let _lock = self.lifecycle_lock()?;
            let _download = self.lock_download_slot()?;
            let i = self
                .release_installation(profile, release)?
                .ok_or_else(|| invalid("Release installation not found"))?;
            for g in &i.generations {
                canonical(&g.id)?;
                let root = self.paths.content.join("apps").join(&g.id);
                if g.state != "Removing" {
                    match fs::symlink_metadata(&root) {
                        Ok(_) => crate::materialization::verify_linux_marker(&root, &g.id)?,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                let tracked: (Option<i64>, Option<String>) = self.conn.query_row(
                    "SELECT process_id,process_start FROM installation_generations WHERE id=?1",
                    [&g.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if runtime_busy(
                    &self.paths.content.join("apps").join(&g.id),
                    tracked.0,
                    tracked.1.as_deref(),
                )? {
                    return Err(invalid("installation is running"));
                }
            }
            let tx = self.conn.transaction()?;
            tx.execute(
                "UPDATE release_installations SET active_generation=NULL WHERE id=?1",
                [&i.id],
            )?;
            tx.execute(
                "UPDATE installation_generations SET state='Removing' WHERE installation_id=?1",
                [&i.id],
            )?;
            tx.commit()?;
            for g in &i.generations {
                delete_owned(&self.paths.content, &self.paths.content.join("apps"), &g.id)?;
                delete_owned(
                    &self.paths.content,
                    &self.paths.content.join("staging"),
                    &format!("manifest-{}", g.id),
                )?;
            }
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM installation_sources WHERE generation_id IN (SELECT id FROM installation_generations WHERE installation_id=?1)",[&i.id])?;
            tx.execute(
                "DELETE FROM installation_generations WHERE installation_id=?1",
                [&i.id],
            )?;
            tx.execute("DELETE FROM release_installations WHERE id=?1", [&i.id])?;
            tx.commit()?;
            Ok(())
        }
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;
    use crate::{library::LibraryPaths, models::InventoryEntry};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::fs;
    use uuid::Uuid;
    fn id() -> String {
        Uuid::new_v4().to_string()
    }
    fn save(lib: &Library, profile: &ServerProfileId, plan: &Value) -> String {
        let id = id();
        let raw = plan.to_string();
        let fingerprint = format!("{:x}", Sha256::digest(raw.as_bytes()));
        lib.conn.execute("INSERT INTO installation_plans(id,profile_id,plan_json,fingerprint) VALUES (?1,?2,?3,?4)",params![id,profile.0,raw,fingerprint]).unwrap();
        id
    }
    fn source(
        lib: &Library,
        profile: &ServerProfileId,
        release: &str,
        bytes: &[u8],
        tree: bool,
    ) -> Value {
        let association = id();
        let job = id();
        let file_id = id();
        let hash = format!("{:x}", Sha256::digest(bytes));
        let entries = vec![InventoryEntry::File {
            path: "original".into(),
            file_id: file_id.clone(),
            size: bytes.len() as u64,
            sha256: hash.clone(),
            executable: false,
        }];
        let artifact = if tree {
            crate::tree::identity(&entries).unwrap()
        } else {
            format!("adfile:sha256:{hash}")
        };
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile.0,
                    artifact,
                    json!({"id":artifact,"kind":if tree{"Tree"}else{"File"},"size":bytes.len()})
                        .to_string()
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5,0)",params![profile.0,association,release,artifact,json!({"id":association,"releaseId":release,"artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at,inventory_json) VALUES (?1,?2,?3,'Completed',0,?4,?5,0,?6)",params![job,profile.0,artifact,association,bytes.len(),if tree{Some(serde_json::to_string(&entries).unwrap())}else{None}]).unwrap();
        if tree {
            lib.conn.execute("INSERT INTO tree_files(job_id,path,file_id,size,sha256,executable,verified) VALUES (?1,'original',?2,?3,?4,0,1)",params![job,file_id,bytes.len(),hash]).unwrap();
        }
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,0,?4)",params![profile.0,artifact,format!("artifacts/{}/{}/{}",profile.0,association,hash),association]).unwrap();
        let object = lib.object(&format!("adfile:sha256:{hash}")).unwrap();
        fs::create_dir_all(object.parent().unwrap()).unwrap();
        fs::write(&object, bytes).unwrap();
        json!({"associationId":association,"artifactId":artifact,"fileId":if tree{Some(file_id)}else{None},"sha256":hash,"size":bytes.len(),"executable":!tree})
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn generation_activation_recovery_launch_and_retention() {
        let temp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths {
            metadata: temp.path().join("metadata"),
            content: temp.path().join("content"),
        };
        let mut lib = Library::open(paths.clone()).unwrap();
        let profile = lib
            .add_profile("fixture", "http://localhost:8787", false)
            .unwrap()
            .id;
        let app = id();
        let release = id();
        let revision = id();
        let association_revision = id();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
        let binary = source(
            &lib,
            &profile,
            &release,
            &fs::read("/bin/true").unwrap(),
            false,
        );
        let data = source(&lib, &profile, &release, b"synthetic", true);
        let plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":crate::launch::host().unwrap(),"selections":{},"unresolved":[],"files":{"bin/renamed":binary,"data/renamed":data},"directories":["bin","data","empty"],"prerequisites":[],"launch":{"method":"NativePortableV1","associationId":binary["associationId"],"entryPointId":"run","revisionId":association_revision,"path":"bin/renamed","workingDirectory":"bin","arguments":[]}});
        let plan_id = save(&lib, &profile, &plan);
        let mut derived = plan.clone();
        for field in ["files", "directories", "prerequisites", "launch"] {
            derived.as_object_mut().unwrap().remove(field);
        }
        derived["recipe"] = json!({"version":1,"releaseId":release,"appId":app,"platform":crate::launch::host().unwrap(),"releases":{(&release):{}},"files":{},"directories":[],"launch":{}});
        let derived_id = save(&lib, &profile, &derived);
        assert!(
            lib.install_saved_plan(&profile, &derived_id, None)
                .unwrap_err()
                .to_string()
                .contains("Derived installation is unsupported")
        );
        assert!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .is_none()
        );
        let installation = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        let first = installation.active_generation.unwrap();
        let payload = installation.generations[0].path.clone();
        assert_eq!(installation.generations[0].state, "Ready");
        assert!(payload.join("empty").is_dir());
        assert_eq!(
            fs::read(payload.join("data/renamed")).unwrap(),
            b"synthetic"
        );
        lib.verify_release_installation(&profile, &release).unwrap();
        assert_eq!(lib.collect_unreferenced_objects().unwrap(), 0);
        assert!(
            lib.play_declared(
                &profile,
                &release,
                binary["associationId"].as_str().unwrap(),
                "run",
                &first
            )
            .unwrap()
            .wait()
            .unwrap()
            .success()
        );
        assert!(
            lib.remove_local_artifact(&profile, data["associationId"].as_str().unwrap())
                .is_err()
        );
        let object = lib
            .object(&format!(
                "adfile:sha256:{}",
                data["sha256"].as_str().unwrap()
            ))
            .unwrap();
        fs::write(payload.join("data/renamed"), b"user change").unwrap();
        assert_eq!(fs::read(&object).unwrap(), b"synthetic");
        assert!(lib.verify_release_installation(&profile, &release).is_err());
        fs::write(&object, b"corrupted").unwrap();
        assert!(lib.install_saved_plan(&profile, &plan_id, None).is_err());
        let failed = lib
            .release_installation(&profile, &release)
            .unwrap()
            .unwrap();
        assert_eq!(failed.active_generation.as_deref(), Some(first.as_str()));
        assert_eq!(
            fs::read(payload.join("data/renamed")).unwrap(),
            b"user change"
        );
        let pending = failed
            .generations
            .iter()
            .find(|g| g.state == "Preparing")
            .unwrap()
            .id
            .clone();
        fs::write(&object, b"synthetic").unwrap();
        let stage = lib
            .paths
            .content
            .join("staging")
            .join(format!("manifest-{pending}"));
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("partial"), b"interrupted").unwrap();
        lib.conn.execute_batch("CREATE TRIGGER fail_activation BEFORE UPDATE OF active_generation ON release_installations WHEN NEW.active_generation IS NOT NULL BEGIN SELECT RAISE(ABORT, 'synthetic activation failure'); END;").unwrap();
        assert!(lib.install_saved_plan(&profile, &plan_id, None).is_err());
        assert!(!stage.exists());
        assert!(
            lib.paths
                .content
                .join("apps")
                .join(&pending)
                .join("payload/bin/renamed")
                .exists()
        );
        assert_eq!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .unwrap()
                .active_generation
                .as_deref(),
            Some(first.as_str())
        );
        lib.conn
            .execute_batch("DROP TRIGGER fail_activation;")
            .unwrap();
        drop(lib);
        let mut lib = Library::open(paths).unwrap();
        let recovered = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        assert_eq!(
            recovered.active_generation.as_deref(),
            Some(pending.as_str())
        );
        lib.verify_release_installation(&profile, &release).unwrap();
        assert!(
            lib.play_declared(
                &profile,
                &release,
                binary["associationId"].as_str().unwrap(),
                "run",
                &first
            )
            .is_err()
        );
        let mut blocked = plan.clone();
        blocked["prerequisites"] = json!([{"id":"required","name":"Inert prerequisite","file":data,"path":"original","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]}]);
        let blocked_id = save(&lib, &profile, &blocked);
        let blocked = lib.install_saved_plan(&profile, &blocked_id, None).unwrap();
        assert_eq!(
            blocked.generations.last().unwrap().state,
            "PrerequisitesPending"
        );
        assert_ne!(
            lib.launch_candidates(&profile, &release).unwrap()[0].status,
            "Ready"
        );
        lib.uninstall_release_installation(&profile, &release)
            .unwrap();
        assert!(!payload.exists());
        assert!(object.exists());
        assert!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .is_none()
        );
        lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        lib.verify_release_installation(&profile, &release).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn windows_shared_generation_activation_and_pending_prerequisites() {
        let temp = tempfile::tempdir().unwrap();
        let mut lib = Library::open(LibraryPaths {
            metadata: temp.path().join("metadata"),
            content: temp.path().join("content"),
        })
        .unwrap();
        let profile = lib
            .add_profile("fixture", "http://localhost:8787", false)
            .unwrap()
            .id;
        let app = id();
        let release = id();
        let revision = id();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
        let f = source(&lib, &profile, &release, b"fixture File", false);
        let t = source(&lib, &profile, &release, b"fixture Tree", true);
        let plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":"windows/x86_64","selections":{},"unresolved":[],"files":{"renamed":f,"data/renamed":t},"directories":["data","empty"],"prerequisites":[],"launch":null});
        let plan_id = save(&lib, &profile, &plan);
        let first = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        assert_eq!(first.generations[0].state, "Ready");
        lib.verify_release_installation(&profile, &release).unwrap();
        assert!(first.generations[0].path.join("empty").is_dir());
        let mut blocked = plan.clone();
        blocked["prerequisites"] = json!([{"id":"required","name":"Inert prerequisite","file":t,"path":"original","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]}]);
        let blocked_id = save(&lib, &profile, &blocked);
        let next = lib.install_saved_plan(&profile, &blocked_id, None).unwrap();
        assert_eq!(next.generations[0].state, "Retired");
        assert_eq!(next.generations[1].state, "PrerequisitesPending");
        lib.verify_release_installation(&profile, &release).unwrap();
        assert!(
            lib.launch_candidates(&profile, &release)
                .unwrap()
                .is_empty()
        );
    }
}
