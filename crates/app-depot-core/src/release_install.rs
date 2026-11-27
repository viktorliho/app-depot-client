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
    pub(crate) fn load_install_plan(
        &self,
        profile: &ServerProfileId,
        id: &str,
    ) -> Result<Plan, LibraryError> {
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

    #[cfg(any(target_os = "linux", windows))]
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
    use crate::launch::LaunchCandidate;
    #[cfg(target_os = "linux")]
    use crate::managed::{delete_owned, process_start, runtime_busy};
    use crate::{
        api::ApiClient,
        installation::Source,
        materialization::{destination_layout, publish_generation, verify_generation},
        models::InventoryEntry,
        transfer::DownloadJob,
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
    // A mapped subtree is suitable only if its complete relative inventory
    // has the original Tree identity, including empty directories and modes.
    fn embedded_root(
        plan: &Plan,
        layout: &[InventoryEntry],
        step: &crate::installation::Prerequisite,
    ) -> Option<String> {
        let id = step.file.file_id.as_ref()?;
        for (dest, source) in plan.files.as_ref()? {
            if source.association_id != step.file.association_id
                || source.artifact_id != step.file.artifact_id
                || source.file_id.as_ref() != Some(id)
            {
                continue;
            }
            let Some(prefix) = dest.strip_suffix(&step.path) else {
                continue;
            };
            if !prefix.is_empty() && !prefix.ends_with('/') {
                continue;
            }
            let entries: Vec<_> = layout
                .iter()
                .filter_map(|entry| {
                    let relative = entry.path().strip_prefix(prefix)?;
                    let mut local = entry.clone();
                    match &mut local {
                        InventoryEntry::File { path, .. } | InventoryEntry::Directory { path } => {
                            *path = relative.to_owned()
                        }
                    }
                    Some(local)
                })
                .collect();
            if crate::tree::identity(&entries).ok().as_ref() == Some(&step.file.artifact_id) {
                return Some(prefix.to_owned());
            }
        }
        None
    }

    struct AttemptSources<'a> {
        inventories: &'a mut BTreeMap<String, Vec<InventoryEntry>>,
        embedded: Option<(&'a std::path::Path, &'a str)>,
        previous: Option<&'a crate::prerequisite::PreparedPrerequisiteSource>,
    }

    enum PreparedAttempt {
        Reused(String),
        Execute(crate::prerequisite::PreparedPrerequisiteSource),
    }

    impl Library {
        #[cfg(target_os = "linux")]
        pub(crate) fn manifest_lifecycle_lock(&self) -> Result<std::fs::File, LibraryError> {
            self.lifecycle_lock()
        }
        #[cfg(windows)]
        pub(crate) fn manifest_lifecycle_lock(&self) -> Result<std::fs::File, LibraryError> {
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
            let mut retained: std::collections::BTreeSet<_> = sources.keys().copied().collect();
            retained.extend(
                plan.prerequisites
                    .iter()
                    .flatten()
                    .map(|p| (p.file.association_id.as_str(), p.file.artifact_id.as_str())),
            );
            for (association, artifact) in &retained {
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
            let pending = existing.as_ref().and_then(|i| {
                i.generations
                    .iter()
                    .find(|g| g.state == "Preparing")
                    .or_else(|| {
                        i.generations
                            .iter()
                            .rev()
                            .find(|g| g.state == "PrerequisitesPending")
                    })
            });
            if pending.is_some_and(|g| g.plan_id != plan_id) {
                return Err(invalid(
                    "retry the pending plan or uninstall before selecting another plan",
                ));
            }
            if pending.is_some_and(|g| g.state == "PrerequisitesPending") {
                return existing.ok_or_else(|| invalid("pending installation missing"));
            }
            let id = pending.map_or_else(|| Uuid::new_v4().to_string(), |g| g.id.clone());
            canonical(&id)?;
            let tx = self.conn.transaction()?;
            tx.execute("INSERT OR IGNORE INTO release_installations(id,profile_id,app_id,release_id) VALUES (?1,?2,?3,?4)",params![install_id,profile.0,plan.app_id,plan.release_id])?;
            if pending.is_none() {
                tx.execute("INSERT INTO installation_generations(id,installation_id,plan_id,state) VALUES (?1,?2,?3,'Preparing')",params![id,install_id,plan_id])?;
            }
            tx.execute("UPDATE installation_generations SET last_error=NULL WHERE id=?1 AND state='Preparing'", [&id])?;
            for (association, artifact) in &retained {
                tx.execute("INSERT OR IGNORE INTO installation_sources(generation_id,association_id,artifact_id) VALUES (?1,?2,?3)",params![id,association,artifact])?;
            }
            tx.commit()?;
            let prepared = (|| -> Result<(), LibraryError> {
                #[cfg(windows)]
                let mut jobs = Vec::new();
                for ((association, artifact), files) in &sources {
                    let job = self.acquire_plan_source(
                        profile,
                        &plan.release_id,
                        association,
                        artifact,
                        client,
                    )?;
                    self.authorize_plan_files(&job, files)?;
                    #[cfg(windows)]
                    jobs.push(job);
                }
                let _download = self.lock_download_slot()?;
                let target = self.paths.content.join("apps").join(&id);
                #[cfg(windows)]
                let metadata = crate::windows_fs::retry_sharing(
                    || fs::symlink_metadata(&target),
                    || format!("symlink_metadata {}", target.display()),
                );
                #[cfg(not(windows))]
                let metadata = fs::symlink_metadata(&target);
                match metadata {
                    Ok(_) => {
                        verify_generation(&target, &id, &layout)?;
                        crate::transfer::sync_dir(&self.paths.content.join("apps"))?;
                        #[cfg(target_os = "linux")]
                        crate::transfer::sync_dir(&self.paths.content.join("staging"))?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => publish_generation(
                        &self.paths.content,
                        &id,
                        &layout,
                        |digest| self.object(&format!("adfile:sha256:{digest}")),
                        #[cfg(windows)]
                        |digest, size| self.repair_install_object(&jobs, digest, size, client),
                    )?,
                    Err(e) => return Err(e.into()),
                }
                let state = if !plan
                    .prerequisites
                    .as_ref()
                    .expect("Standard plan")
                    .is_empty()
                {
                    "PrerequisitesPending"
                } else {
                    "Ready"
                };
                let tx = self.conn.transaction()?;

                if tx.execute("UPDATE installation_generations SET state=?2,last_error=NULL WHERE id=?1 AND installation_id=?3 AND state='Preparing'",params![id,state,install_id])? != 1 {return Err(invalid("pending generation changed before activation"));}
                if state == "Ready" {
                    Self::activate_generation(&tx, &install_id, &id)?;
                }
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

        fn activate_generation(
            tx: &rusqlite::Transaction<'_>,
            installation: &str,
            generation: &str,
        ) -> Result<(), LibraryError> {
            tx.execute("UPDATE installation_generations SET state='Retired' WHERE id=(SELECT active_generation FROM release_installations WHERE id=?1) AND id<>?2",params![installation,generation])?;
            tx.execute(
                "UPDATE release_installations SET active_generation=?2 WHERE id=?1",
                params![installation, generation],
            )?;
            Ok(())
        }

        fn prerequisite_inventory(
            &self,
            job: &DownloadJob,
        ) -> Result<Vec<InventoryEntry>, LibraryError> {
            let entries = self
                .tree_inventory(&job.id)?
                .ok_or_else(|| invalid("missing verified Tree inventory"))?;
            if crate::tree::identity(&entries)? != job.artifact_id {
                return Err(invalid("Tree identity mismatch"));
            }
            // Each member must still have a durable verified CAS reference.
            let mut stmt = self.conn.prepare("SELECT path,file_id,size,sha256,executable,verified FROM tree_files WHERE job_id=?1")?;
            let indexed = stmt
                .query_map([&job.id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (
                            r.get::<_, String>(1)?,
                            r.get::<_, u64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, bool>(4)?,
                            r.get::<_, bool>(5)?,
                        ),
                    ))
                })?
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            let mut count = 0;
            for entry in &entries {
                if let InventoryEntry::File {
                    path,
                    file_id,
                    size,
                    sha256,
                    executable,
                } = entry
                {
                    count += 1;
                    if indexed
                        .get(path)
                        .is_none_or(|(id, len, digest, exec, verified)| {
                            id != file_id
                                || len != size
                                || digest != sha256
                                || exec != executable
                                || !verified
                        })
                    {
                        return Err(invalid(
                            "Tree source member lacks a matching verified CAS reference",
                        ));
                    }
                }
            }
            if indexed.len() != count {
                return Err(invalid(
                    "Tree source member index differs from verified inventory",
                ));
            }
            Ok(entries)
        }

        fn prepare_prerequisite_source(
            &self,
            job: &DownloadJob,
            p: &crate::installation::Prerequisite,
            attempt: &str,
            prior_success: Option<String>,
            sources: AttemptSources<'_>,
            progress: &mut dyn FnMut(&str),
        ) -> Result<PreparedAttempt, LibraryError> {
            let _download = self.lock_download_slot()?;
            if !self.source_in_use(&job.profile_id, &job.association_id, &job.artifact_id)? {
                return Err(invalid(
                    "prerequisite source has no durable generation reference",
                ));
            }
            let registered: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM download_jobs j JOIN verified_content v ON (j.profile_id,j.association_id,j.artifact_id)=(v.profile_id,v.association_id,v.artifact_id) WHERE j.id=?1 AND j.state='Completed')",[&job.id],|r|r.get(0))?;
            if !registered {
                return Err(invalid(
                    "completed prerequisite Artifact reference disappeared",
                ));
            }
            let standalone;
            let (entries, relative_file) = if let Some(id) = &p.file.file_id {
                use std::collections::btree_map::Entry;
                let entries = match sources.inventories.entry(job.id.clone()) {
                    Entry::Occupied(slot) => slot.into_mut(),
                    Entry::Vacant(slot) => slot.insert(self.prerequisite_inventory(job)?),
                };
                let matches_member = entries.iter().any(|entry| match entry {
                    InventoryEntry::Directory { .. } => false,
                    InventoryEntry::File {
                        path,
                        file_id,
                        size,
                        sha256,
                        executable,
                    } => {
                        path == &p.path
                            && file_id == id
                            && *size == p.file.size
                            && sha256 == &p.file.sha256
                            && *executable == p.file.executable
                    }
                });
                if !matches_member {
                    return Err(invalid(
                        "prerequisite path does not match verified Tree member",
                    ));
                }
                (entries.as_slice(), p.path.as_str())
            } else {
                if !p.path.is_empty()
                    || p.file.size != job.expected_size
                    || job.artifact_id != format!("adfile:sha256:{}", p.file.sha256)
                {
                    return Err(invalid("saved prerequisite does not match verified File"));
                }
                // File Artifacts have no path in InstallationManifestV1.
                // This is an opaque local name, not executable-type inference.
                standalone = [InventoryEntry::File {
                    path: "prerequisite".into(),
                    file_id: String::new(),
                    size: p.file.size,
                    sha256: p.file.sha256.clone(),
                    executable: p.file.executable,
                }];
                (&standalone[..], "prerequisite")
            };
            if let Some(origin) = prior_success {
                progress("Reusing recorded prerequisite completion");
                return Ok(PreparedAttempt::Reused(origin));
            }
            if let Some((payload, generation)) = sources.embedded {
                return crate::prerequisite::PreparedPrerequisiteSource::new(
                    generation,
                    payload.to_owned(),
                    relative_file,
                )
                .map(PreparedAttempt::Execute);
            }
            crate::materialization::publish_prerequisite_source(
                &self.paths.content,
                attempt,
                entries,
                relative_file,
                sources.previous,
                |digest| self.object(&format!("adfile:sha256:{digest}")),
                progress,
            )
            .map(PreparedAttempt::Execute)
        }

        fn pending_prerequisite_generation(
            &self,
            profile: &ServerProfileId,
            generation: &str,
        ) -> Result<(String, String, String, Plan), LibraryError> {
            canonical(generation)?;
            let (installation, plan_id, fingerprint): (String, String, String) = self.conn.query_row(
                "SELECT i.id,g.plan_id,p.fingerprint FROM installation_generations g JOIN release_installations i ON i.id=g.installation_id JOIN installation_plans p ON p.id=g.plan_id WHERE g.id=?1 AND i.profile_id=?2 AND p.profile_id=?2 AND g.state='PrerequisitesPending' AND NOT EXISTS(SELECT 1 FROM installation_generations newer WHERE newer.installation_id=g.installation_id AND newer.rowid>g.rowid AND newer.state IN ('Preparing','PrerequisitesPending'))",
                params![generation, profile.0], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))
            ).optional()?.ok_or_else(|| invalid("pending prerequisite generation not found"))?;
            let plan = self.load_install_plan(profile, &plan_id)?;
            if crate::launch::host() != Some(&plan.platform) {
                return Err(invalid("installation platform does not match this host"));
            }
            Ok((installation, plan_id, fingerprint, plan))
        }

        /// Exact immutable saved plan (including unresolved notes) for review.
        pub fn prerequisite_requirements(
            &self,
            profile: &ServerProfileId,
            plan_id: &str,
        ) -> Result<crate::installation::SavedPlan, LibraryError> {
            self.load_install_plan(profile, plan_id)?;
            self.saved_plans(profile)?
                .into_iter()
                .find(|p| p.id == plan_id)
                .ok_or_else(|| invalid("saved plan not found"))
        }

        /// Durable consent rounds and outcomes for a profile-owned generation.
        pub fn prerequisite_progress(
            &self,
            profile: &ServerProfileId,
            generation: &str,
        ) -> Result<serde_json::Value, LibraryError> {
            use serde_json::{Value, json};
            canonical(generation)?;
            // If the lifecycle lock is free, no coordinator can still be
            // recording an outcome for this generation. A surviving child may
            // nevertheless have external effects, so never infer its result.
            let lock = self.manifest_lifecycle_lock().ok();
            let (plan_id,state,last_error): (String,String,Option<String>) = self.conn.query_row("SELECT g.plan_id,g.state,g.last_error FROM installation_generations g JOIN release_installations i ON i.id=g.installation_id WHERE g.id=?1 AND i.profile_id=?2",params![generation,profile.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||invalid("generation not found"))?;
            if lock.is_some() {
                let tx = self.conn.unchecked_transaction()?;
                tx.execute("UPDATE prerequisite_attempts SET state='Ambiguous',detail='Interrupted before outcome was recorded',finished_at=unixepoch() WHERE state='Running' AND consent_id IN (SELECT id FROM prerequisite_consents WHERE generation_id=?1)",[generation])?;
                tx.execute("UPDATE prerequisite_consents SET state='Finished' WHERE generation_id=?1 AND state='Running'",[generation])?;
                tx.commit()?;
            }
            let saved = self.prerequisite_requirements(profile, &plan_id)?;
            let mut rounds = Vec::new();
            let mut consents = self.conn.prepare("SELECT id,fingerprint,selected_json,state,created_at,retry_authorized FROM prerequisite_consents WHERE generation_id=?1 ORDER BY rowid")?;
            let mut rows = consents.query([generation])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let selected: String = row.get(2)?;
                let mut attempts = self.conn.prepare("SELECT id,prerequisite_id,state,exit_code,detail,started_at,finished_at FROM prerequisite_attempts WHERE consent_id=?1 ORDER BY rowid")?;
                let mut outcomes = attempts.query_map([&id], |a| Ok(json!({"id":a.get::<_,String>(0)?,"prerequisiteId":a.get::<_,String>(1)?,"state":a.get::<_,String>(2)?,"exitCode":a.get::<_,Option<i32>>(3)?,"detail":a.get::<_,Option<String>>(4)?,"startedAt":a.get::<_,i64>(5)?,"finishedAt":a.get::<_,Option<i64>>(6)?})))?.collect::<Result<Vec<Value>,_>>()?;
                for outcome in &mut outcomes {
                    let attempt = outcome["id"]
                        .as_str()
                        .ok_or_else(|| invalid("invalid prerequisite attempt ID"))?;
                    let workspace = crate::materialization::prerequisite_workspace(
                        &self.paths.content,
                        attempt,
                    )?;
                    if lock.is_some()
                        && matches!(
                            outcome["state"].as_str(),
                            Some("Succeeded" | "Failed" | "Cancelled" | "Reused")
                        )
                        && let Err(e) = crate::materialization::cleanup_prerequisite_workspace(
                            &self.paths.content,
                            attempt,
                        )
                    {
                        outcome["sourceCleanupError"] = json!(e.to_string());
                    }
                    match fs::symlink_metadata(&workspace) {
                        Ok(_) => outcome["sourceWorkspace"] = json!(workspace),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => outcome["sourceCleanupError"] = json!(e.to_string()),
                    }
                }
                rounds.push(json!({"id":id,"fingerprint":row.get::<_,String>(1)?,"selected":serde_json::from_str::<Value>(&selected)?,"state":row.get::<_,String>(3)?,"createdAt":row.get::<_,i64>(4)?,"retryAuthorized":row.get::<_,bool>(5)?,"attempts":outcomes}));
            }
            Ok(
                json!({"generationId":generation,"state":state,"lastError":last_error,"savedPlan":saved,"consents":rounds}),
            )
        }

        fn validate_prerequisite_selection(
            plan: &Plan,
            selected: &[String],
        ) -> Result<(), LibraryError> {
            let requirements = plan.prerequisites.as_deref().unwrap_or_default();
            let choices: std::collections::HashSet<_> = selected.iter().collect();
            if choices.len() != selected.len()
                || selected
                    .iter()
                    .any(|id| !requirements.iter().any(|p| &p.id == id))
                || requirements
                    .iter()
                    .any(|p| !p.optional && !choices.contains(&p.id))
            {
                return Err(invalid(
                    "select every required prerequisite and only declared IDs",
                ));
            }
            Ok(())
        }

        /// Explicitly authorize one processing round. Retry must be explicitly set.
        /// The caller must present the fingerprint and exact selected IDs.
        pub fn confirm_prerequisites(
            &mut self,
            profile: &ServerProfileId,
            generation: &str,
            fingerprint: &str,
            selected: &[String],
            retry: bool,
        ) -> Result<String, LibraryError> {
            let _lock = self.manifest_lifecycle_lock()?;
            let (_, _, expected, plan) =
                self.pending_prerequisite_generation(profile, generation)?;
            if fingerprint != expected {
                return Err(invalid("saved plan fingerprint changed"));
            }
            let attempted: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM prerequisite_consents WHERE generation_id=?1 AND state IN ('Running','Finished','Superseded') AND (state<>'Superseded' OR EXISTS(SELECT 1 FROM prerequisite_attempts WHERE consent_id=prerequisite_consents.id)))",[generation],|r|r.get(0))?;
            if attempted && !retry {
                return Err(invalid("explicit retry authorization required"));
            }
            Self::validate_prerequisite_selection(&plan, selected)?;
            let id = Uuid::new_v4().to_string();
            let tx = self.conn.transaction()?;
            tx.execute("UPDATE prerequisite_attempts SET state='Ambiguous',detail='Interrupted before outcome was recorded',finished_at=unixepoch() WHERE state='Running' AND consent_id IN (SELECT id FROM prerequisite_consents WHERE generation_id=?1)",[generation])?;
            tx.execute("UPDATE prerequisite_consents SET state='Superseded' WHERE generation_id=?1 AND state IN ('Confirmed','Running')",[generation])?;
            tx.execute("INSERT INTO prerequisite_consents(id,generation_id,fingerprint,selected_json,retry_authorized,state) VALUES (?1,?2,?3,?4,?5,'Confirmed')",params![id,generation,fingerprint,serde_json::to_string(selected)?,retry])?;
            tx.commit()?;
            Ok(id)
        }

        /// Process the confirmed selection in manifest order. A confirmation is
        /// single use; an interrupted round never silently starts another installer.
        pub fn process_prerequisites(
            &mut self,
            profile: &ServerProfileId,
            generation: &str,
            consent: &str,
            client: Option<&ApiClient>,
            executor: &mut impl crate::prerequisite::PrerequisiteExecutor,
        ) -> Result<ReleaseInstallation, LibraryError> {
            self.process_prerequisites_with_progress(
                profile,
                generation,
                consent,
                client,
                executor,
                &mut |_| {},
            )
        }

        pub fn process_prerequisites_with_progress(
            &mut self,
            profile: &ServerProfileId,
            generation: &str,
            consent: &str,
            client: Option<&ApiClient>,
            executor: &mut impl crate::prerequisite::PrerequisiteExecutor,
            progress: &mut dyn FnMut(&str),
        ) -> Result<ReleaseInstallation, LibraryError> {
            use crate::prerequisite::{PrerequisiteOutcome, PrerequisiteRequest};
            let _lock = self.manifest_lifecycle_lock()?;
            let (installation, _, fingerprint, plan) =
                self.pending_prerequisite_generation(profile, generation)?;
            let selected: String = self.conn.query_row("SELECT selected_json FROM prerequisite_consents WHERE id=?1 AND generation_id=?2 AND fingerprint=?3 AND state='Confirmed'",params![consent,generation,fingerprint],|r|r.get(0)).optional()?.ok_or_else(|| invalid("fresh matching prerequisite consent required"))?;
            let selected: Vec<String> = serde_json::from_str(&selected)?;
            Self::validate_prerequisite_selection(&plan, &selected)?;
            // Validate every durable attempt identity before any new source publication.
            self.prerequisite_attempt_ids(generation)?;

            let root = self.paths.content.join("apps").join(generation);
            let payload = root.join("payload");
            let layout = destination_layout(&plan)?;
            let mut embedded_roots = BTreeMap::new();

            progress("Checking installed application files");
            let mut checks = crate::materialization::LayoutChecks::default();
            crate::materialization::check_generation(&root, generation, &layout, &mut checks)?;
            let mut inventories = BTreeMap::new();
            let mut acquired = BTreeMap::new();
            for p in plan
                .prerequisites
                .iter()
                .flatten()
                .filter(|p| selected.contains(&p.id))
            {
                if let std::collections::btree_map::Entry::Vacant(slot) =
                    acquired.entry((&p.file.association_id, &p.file.artifact_id))
                {
                    progress(&format!("Acquiring source for {}", p.name));
                    slot.insert(self.acquire_plan_source(
                        profile,
                        &plan.release_id,
                        &p.file.association_id,
                        &p.file.artifact_id,
                        client,
                    )?);
                }
            }
            let tx = self.conn.transaction()?;
            for p in plan.prerequisites.iter().flatten() {
                tx.execute("INSERT OR IGNORE INTO installation_sources(generation_id,association_id,artifact_id) VALUES (?1,?2,?3)",params![generation,p.file.association_id,p.file.artifact_id])?;
            }
            tx.execute(
                "UPDATE prerequisite_consents SET state='Running' WHERE id=?1",
                [consent],
            )?;
            tx.commit()?;
            let mut steps = plan.prerequisites.iter().flatten().peekable();
            let mut retained: Option<(String, crate::prerequisite::PreparedPrerequisiteSource)> =
                None;
            while let Some(p) = steps.next() {
                let attempt = Uuid::new_v4().to_string();
                if !selected.contains(&p.id) {
                    self.conn.execute("INSERT INTO prerequisite_attempts(id,consent_id,prerequisite_id,state,finished_at) VALUES (?1,?2,?3,'Declined',unixepoch())",params![attempt,consent,p.id])?;
                    continue;
                }
                self.conn.execute("INSERT INTO prerequisite_attempts(id,consent_id,prerequisite_id,state) VALUES (?1,?2,?3,'Running')",params![attempt,consent,p.id])?;
                let previous: Option<(String, i32)> = self.conn.query_row(
                    "WITH latest AS (
                        SELECT CASE a.state WHEN 'Succeeded' THEN a.id WHEN 'Reused' THEN a.detail END AS origin
                        FROM prerequisite_attempts a JOIN prerequisite_consents c ON c.id=a.consent_id
                        WHERE c.generation_id=?1 AND c.fingerprint=?2 AND a.prerequisite_id=?3 AND a.id<>?4
                        ORDER BY a.rowid DESC LIMIT 1
                    )
                    SELECT a.id,a.exit_code FROM latest JOIN prerequisite_attempts a ON a.id=latest.origin
                    JOIN prerequisite_consents c ON c.id=a.consent_id
                    WHERE c.generation_id=?1 AND c.fingerprint=?2 AND a.prerequisite_id=?3
                    AND a.state='Succeeded' AND a.finished_at IS NOT NULL AND a.exit_code IS NOT NULL",
                    params![generation,fingerprint,p.id,attempt], |r| Ok((r.get(0)?,r.get(1)?))
                ).optional()?;
                let prior_success = previous
                    .filter(|(_, code)| p.success_exit_codes.contains(code))
                    .map(|(id, _)| id);
                let source = (|| -> Result<PreparedAttempt, LibraryError> {
                    let job = &acquired[&(&p.file.association_id, &p.file.artifact_id)];
                    let embedded = embedded_roots
                        .entry((&p.file.association_id, &p.file.artifact_id))
                        .or_insert_with(|| {
                            embedded_root(&plan, &layout, p).map(|prefix| payload.join(prefix))
                        })
                        .as_deref();
                    if embedded.is_some() && prior_success.is_none() {
                        crate::materialization::check_generation(
                            &root,
                            generation,
                            &layout,
                            &mut checks,
                        )?;
                    }
                    self.prepare_prerequisite_source(
                        job,
                        p,
                        &attempt,
                        prior_success,
                        AttemptSources {
                            inventories: &mut inventories,
                            embedded: embedded.map(|root| (root, generation)),
                            previous: retained.as_ref().map(|(_, source)| source),
                        },
                        &mut |phase| progress(&format!("{}: {phase}", p.name)),
                    )
                })();
                if let Some((owner, _)) = retained.take() {
                    crate::materialization::cleanup_prerequisite_workspace(
                        &self.paths.content,
                        &owner,
                    )?;
                }
                let source = match source {
                    Ok(PreparedAttempt::Execute(source)) => source,
                    Ok(PreparedAttempt::Reused(origin)) => {
                        self.conn.execute("UPDATE prerequisite_attempts SET state='Reused',detail=?2,finished_at=unixepoch() WHERE id=?1", params![attempt,origin])?;
                        continue;
                    }
                    Err(e) => {
                        let tx = self.conn.transaction()?;
                        tx.execute("UPDATE prerequisite_attempts SET state='Failed',detail=?2,finished_at=unixepoch() WHERE id=?1",params![attempt,e.to_string()])?;
                        tx.execute(
                            "UPDATE prerequisite_consents SET state='Finished' WHERE id=?1",
                            [consent],
                        )?;
                        tx.execute(
                            "UPDATE installation_generations SET last_error=?2 WHERE id=?1",
                            params![generation, format!("Prerequisite {}: {e}", p.id)],
                        )?;
                        tx.commit()?;
                        let _ = crate::materialization::cleanup_prerequisite_workspace(
                            &self.paths.content,
                            &attempt,
                        );
                        return self
                            .release_installation(profile, &plan.release_id)?
                            .ok_or_else(|| invalid("installation missing"));
                    }
                };
                let request = PrerequisiteRequest {
                    generation_id: generation,
                    attempt_id: &attempt,
                    payload: &payload,
                    source: &source,
                    prerequisite: p,
                };
                progress(&format!(
                    "Running {} (waiting for Windows or installer)",
                    p.name
                ));
                let outcome = executor.execute(&request);

                let (state, code, detail) = match outcome {
                    PrerequisiteOutcome::Exited(code) if p.success_exit_codes.contains(&code) => {
                        ("Succeeded", Some(code), None)
                    }
                    PrerequisiteOutcome::Exited(code) => (
                        "Failed",
                        Some(code),
                        Some("Unexpected exit code".to_owned()),
                    ),
                    PrerequisiteOutcome::Failed(reason) => ("Failed", None, Some(reason)),
                    PrerequisiteOutcome::Cancelled => ("Cancelled", None, None),
                    PrerequisiteOutcome::Ambiguous(reason) => ("Ambiguous", None, Some(reason)),
                };
                self.conn.execute("UPDATE prerequisite_attempts SET state=?2,exit_code=?3,detail=?4,finished_at=unixepoch() WHERE id=?1",params![attempt,state,code,detail])?;
                if state == "Succeeded"
                    && source.owner_id != generation
                    && steps.peek().is_some_and(|next| {
                        selected.contains(&next.id)
                            && next.file.artifact_id == p.file.artifact_id
                            && next.file.association_id == p.file.association_id
                    })
                {
                    retained = Some((attempt, source));
                } else if state != "Ambiguous" {
                    progress(&format!("Cleaning up {}", p.name));
                    let _ = crate::materialization::cleanup_prerequisite_workspace(
                        &self.paths.content,
                        &attempt,
                    );
                }
                if state != "Succeeded" {
                    let tx = self.conn.transaction()?;
                    tx.execute(
                        "UPDATE prerequisite_consents SET state='Finished' WHERE id=?1",
                        [consent],
                    )?;
                    tx.execute(
                        "UPDATE installation_generations SET last_error=?2 WHERE id=?1",
                        params![generation, format!("Prerequisite {}: {state}", p.id)],
                    )?;
                    tx.commit()?;
                    return self
                        .release_installation(profile, &plan.release_id)?
                        .ok_or_else(|| invalid("installation missing"));
                }
            }
            progress("Verifying installed application files before activation");
            let activation = (|| -> Result<(), LibraryError> {
                crate::materialization::check_generation(&root, generation, &layout, &mut checks)?;
                // Activation and completion are atomic; an interrupted transaction
                // leaves the old generation active and requires fresh authorization.
                let tx = self.conn.transaction()?;
                tx.execute(
                    "UPDATE prerequisite_consents SET state='Finished' WHERE id=?1",
                    [consent],
                )?;
                tx.execute("UPDATE installation_generations SET state='Ready',last_error=NULL WHERE id=?1 AND state='PrerequisitesPending'",[generation])?;
                Self::activate_generation(&tx, &installation, generation)?;
                tx.commit()?;
                Ok(())
            })();
            if let Err(e) = activation {
                self.conn.execute(
                    "UPDATE installation_generations SET last_error=?2 WHERE id=?1",
                    params![generation, e.to_string()],
                )?;
                return Err(e);
            }
            self.release_installation(profile, &plan.release_id)?
                .ok_or_else(|| invalid("installation missing"))
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
            let mut job = if let Some(job) = self.active_association_job(profile, association)? {
                job
            } else if let Some(id) = self.failed_source_job(profile, association, artifact)? {
                self.job(&id)?
            } else if artifact.starts_with("adfile:") {
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
                self.run_file_job(client, &job.id, None)
            } else {
                self.run_tree_job(client, &job.id, None)
            }
            .map_err(|e| {
                invalid(format!(
                    "download job {} ({}), source {association}, artifact {artifact}: {e}",
                    job.id, job.state
                ))
            })?;
            if job.state != "Completed" {
                return Err(invalid(format!(
                    "download job {} stopped in {} for source {association}",
                    job.id, job.state
                )));
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
        pub(crate) fn active_release_generation(
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
            #[cfg(target_os = "linux")]
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
            #[cfg(windows)]
            if ready {
                let root = self.paths.content.join("apps").join(&g.id);
                let (target, work) = crate::launch::windows_paths(
                    false,
                    candidate.target.as_deref().unwrap(),
                    candidate.working_directory.as_deref().unwrap(),
                );
                let occupancy = self.windows_generation_occupancy(&g.id)?;
                if occupancy != crate::windows_process::ProcessState::Idle {
                    candidate.status =
                        if occupancy == crate::windows_process::ProcessState::Running {
                            "Running"
                        } else {
                            "ProcessUnknown"
                        }
                        .into();
                    candidate.reason = "Managed generation occupancy is not idle".into();
                } else if let Err(error) =
                    crate::windows_fs::bind_launch_target(&root, &g.id, &target, &work)
                {
                    candidate.status = "Unavailable".into();
                    candidate.reason = error.to_string();
                }
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
        /// Durable attempt IDs own source staging paths; the caller must hold
        /// the lifecycle lock and establish quiescence before removing them.
        pub(crate) fn prerequisite_attempt_ids(
            &self,
            generation: &str,
        ) -> Result<Vec<String>, LibraryError> {
            canonical(generation)?;
            let mut stmt = self.conn.prepare("SELECT a.id FROM prerequisite_attempts a JOIN prerequisite_consents c ON c.id=a.consent_id WHERE c.generation_id=?1 ORDER BY a.rowid")?;
            let ids = stmt
                .query_map([generation], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in &ids {
                canonical(id)?;
            }
            Ok(ids)
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
                for attempt in self.prerequisite_attempt_ids(&g.id)? {
                    let workspace = crate::materialization::prerequisite_workspace(
                        &self.paths.content,
                        &attempt,
                    )?;
                    if runtime_busy(&workspace, None, None)? {
                        return Err(invalid("prerequisite source workspace is running"));
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
                for attempt in self.prerequisite_attempt_ids(&g.id)? {
                    crate::materialization::cleanup_prerequisite_workspace(
                        &self.paths.content,
                        &attempt,
                    )?;
                }
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
        source_layout(
            lib,
            profile,
            release,
            bytes,
            tree.then_some("original"),
            None,
        )
    }

    fn source_layout(
        lib: &Library,
        profile: &ServerProfileId,
        release: &str,
        bytes: &[u8],
        tree_path: Option<&str>,
        support: Option<(&str, &[u8])>,
    ) -> Value {
        let association = id();
        let job = id();
        let file_id = id();
        let hash = format!("{:x}", Sha256::digest(bytes));
        let tree = tree_path.is_some();
        let path = tree_path.unwrap_or("original");
        let mut entries = vec![InventoryEntry::File {
            path: path.into(),
            file_id: file_id.clone(),
            size: bytes.len() as u64,
            sha256: hash.clone(),
            executable: false,
        }];
        let mut objects = vec![(hash.clone(), bytes)];
        if let Some((support_path, support_bytes)) = support {
            let support_hash = format!("{:x}", Sha256::digest(support_bytes));
            entries.push(InventoryEntry::File {
                path: support_path.into(),
                file_id: id(),
                size: support_bytes.len() as u64,
                sha256: support_hash.clone(),
                executable: false,
            });
            objects.push((support_hash, support_bytes));
            entries.push(InventoryEntry::Directory {
                path: "empty".into(),
            });
        }
        let mut dirs = std::collections::BTreeSet::new();
        for entry in &entries {
            let mut relative = entry.path();
            while let Some((parent, _)) = relative.rsplit_once('/') {
                dirs.insert(parent.to_owned());
                relative = parent;
            }
        }
        entries.extend(
            dirs.into_iter()
                .map(|path| InventoryEntry::Directory { path }),
        );
        let size: usize = objects.iter().map(|(_, bytes)| bytes.len()).sum();
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
                    json!({"id":artifact,"kind":if tree{"Tree"}else{"File"},"size":size})
                        .to_string()
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5,0)",params![profile.0,association,release,artifact,json!({"id":association,"releaseId":release,"artifactId":artifact}).to_string()]).unwrap();
        lib.conn.execute("INSERT INTO download_jobs(id,profile_id,artifact_id,state,created_at,association_id,expected_size,updated_at,inventory_json) VALUES (?1,?2,?3,'Completed',0,?4,?5,0,?6)",params![job,profile.0,artifact,association,size,if tree{Some(serde_json::to_string(&entries).unwrap())}else{None}]).unwrap();
        if tree {
            for entry in &entries {
                if let InventoryEntry::File {
                    path,
                    file_id,
                    size,
                    sha256,
                    executable,
                } = entry
                {
                    lib.conn.execute("INSERT INTO tree_files(job_id,path,file_id,size,sha256,executable,progress,verified) VALUES (?1,?2,?3,?4,?5,?6,?4,1)",params![job,path,file_id,size,sha256,executable]).unwrap();
                }
            }
        }
        lib.conn.execute("INSERT INTO verified_content(profile_id,artifact_id,local_ref,verified_at,association_id) VALUES (?1,?2,?3,0,?4)",params![profile.0,artifact,format!("artifacts/{}/{}/{}",profile.0,association,hash),association]).unwrap();
        for (digest, bytes) in objects {
            let object = lib.object(&format!("adfile:sha256:{digest}")).unwrap();
            fs::create_dir_all(object.parent().unwrap()).unwrap();
            fs::write(&object, bytes).unwrap();
        }
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
        fs::create_dir_all(&stage).unwrap();
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
        assert_eq!(blocked.active_generation.as_deref(), Some(pending.as_str()));
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
    #[test]
    fn prerequisite_consent_order_outcomes_and_recoverable_activation() {
        use crate::prerequisite::{PrerequisiteExecutor, PrerequisiteOutcome, PrerequisiteRequest};
        use std::collections::VecDeque;
        struct Mock {
            outcomes: VecDeque<PrerequisiteOutcome>,
            seen: Vec<String>,
        }
        impl PrerequisiteExecutor for Mock {
            fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
                assert!(request.payload.is_dir());
                assert_eq!(fs::read(&request.source.file).unwrap(), b"installer");
                self.seen.push(request.prerequisite.id.clone());
                self.outcomes.pop_front().expect("unexpected execution")
            }
        }
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
        let (app, release, revision) = (id(), id(), id());
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
        let payload = source(&lib, &profile, &release, b"payload", false);
        let installer = source(&lib, &profile, &release, b"installer", true);
        let mut plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":crate::launch::host().unwrap(),"selections":{},"unresolved":[],"files":{"payload":payload},"directories":[],"prerequisites":[],"launch":null});
        let old = lib
            .install_saved_plan(&profile, &save(&lib, &profile, &plan), None)
            .unwrap()
            .active_generation
            .unwrap();
        plan["prerequisites"] = json!([
            {"id":"first","name":"First","file":installer,"path":"original","arguments":["literal argument"],"optional":false,"evidence":"fixture","successExitCodes":[7]},
            {"id":"optional","name":"Optional","file":installer,"path":"original","arguments":[],"optional":true,"evidence":"fixture","successExitCodes":[0]},
            {"id":"last","name":"Last","file":installer,"path":"original","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]}
        ]);
        let plan_id = save(&lib, &profile, &plan);
        let pending = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        let generation = pending.generations.last().unwrap().id.clone();
        assert_eq!(pending.active_generation.as_deref(), Some(old.as_str()));
        assert_eq!(
            pending.generations.last().unwrap().state,
            "PrerequisitesPending"
        );
        lib.conn.execute_batch("DROP TABLE prerequisite_attempts; DROP TABLE prerequisite_consents; PRAGMA user_version=7;").unwrap();
        drop(lib);
        let mut lib = Library::open(paths.clone()).unwrap();
        assert_eq!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .unwrap()
                .active_generation
                .as_deref(),
            Some(old.as_str())
        );
        assert_eq!(
            lib.prerequisite_progress(&profile, &generation).unwrap()["consents"],
            json!([])
        );
        let saved = lib.prerequisite_requirements(&profile, &plan_id).unwrap();
        let fingerprint = saved.fingerprint;
        let selected = vec!["last".to_owned(), "optional".to_owned(), "first".to_owned()];
        let required = vec!["first".into(), "last".into()];
        assert!(
            lib.confirm_prerequisites(&profile, &generation, "wrong", &selected, false)
                .is_err()
        );
        assert!(
            lib.confirm_prerequisites(
                &profile,
                &generation,
                &fingerprint,
                &["first".into()],
                false
            )
            .is_err()
        );
        let mut mock = Mock {
            outcomes: VecDeque::from([
                PrerequisiteOutcome::Exited(7),
                PrerequisiteOutcome::Cancelled,
            ]),
            seen: vec![],
        };
        assert!(
            lib.process_prerequisites(&profile, &generation, "missing", None, &mut mock)
                .is_err()
        );
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fingerprint, &selected, false)
            .unwrap();
        let result = lib
            .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(mock.seen, ["first", "optional"]);
        assert_eq!(result.active_generation.as_deref(), Some(old.as_str()));
        drop(lib);
        let mut lib = Library::open(paths.clone()).unwrap();
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fingerprint, &selected, true)
            .unwrap();
        mock.seen.clear();
        mock.outcomes.extend([
            PrerequisiteOutcome::Exited(0),
            PrerequisiteOutcome::Exited(9),
        ]);
        let result = lib
            .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(mock.seen, ["optional", "last"]);
        assert_eq!(result.active_generation.as_deref(), Some(old.as_str()));
        assert!(
            lib.process_prerequisites(&profile, &generation, &consent, None, &mut mock)
                .is_err()
        );
        assert!(
            lib.confirm_prerequisites(&profile, &generation, &fingerprint, &required, false)
                .is_err()
        );
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert_eq!(progress["consents"][0]["attempts"][0]["exitCode"], 7);
        assert_eq!(progress["consents"][1]["attempts"][2]["state"], "Failed");
        assert_eq!(progress["consents"][1]["attempts"][2]["exitCode"], 9);
        drop(lib);
        let mut lib = Library::open(paths.clone()).unwrap();
        for outcome in [
            PrerequisiteOutcome::Cancelled,
            PrerequisiteOutcome::Ambiguous("unknown".into()),
            PrerequisiteOutcome::Failed("unavailable".into()),
        ] {
            let consent = lib
                .confirm_prerequisites(&profile, &generation, &fingerprint, &required, true)
                .unwrap();
            let mut mock = Mock {
                outcomes: VecDeque::from([outcome]),
                seen: vec![],
            };
            let result = lib
                .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
                .unwrap();
            assert_eq!(mock.seen, ["last"]);
            assert_eq!(result.active_generation.as_deref(), Some(old.as_str()));
        }
        // Fail the activation transaction after successful durable outcomes.
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fingerprint, &required, true)
            .unwrap();
        lib.conn.execute_batch("CREATE TRIGGER fail_prereq_activation BEFORE UPDATE OF active_generation ON release_installations BEGIN SELECT RAISE(ABORT,'synthetic crash'); END;").unwrap();
        let mut mock = Mock {
            outcomes: VecDeque::from([PrerequisiteOutcome::Exited(0)]),
            seen: vec![],
        };
        assert!(
            lib.process_prerequisites(&profile, &generation, &consent, None, &mut mock)
                .is_err()
        );
        assert_eq!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .unwrap()
                .active_generation
                .as_deref(),
            Some(old.as_str())
        );
        lib.conn
            .execute_batch("DROP TRIGGER fail_prereq_activation;")
            .unwrap();
        drop(lib);
        let mut lib = Library::open(paths).unwrap();
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fingerprint, &required, true)
            .unwrap();
        let mut mock = Mock {
            outcomes: VecDeque::new(),
            seen: vec![],
        };
        let mut phases = Vec::new();
        let result = lib
            .process_prerequisites_with_progress(
                &profile,
                &generation,
                &consent,
                None,
                &mut mock,
                &mut |phase| phases.push(phase.to_owned()),
            )
            .unwrap();
        assert_eq!(
            phases
                .iter()
                .filter(|phase| phase.contains("Reusing recorded prerequisite completion"))
                .count(),
            2
        );
        assert!(
            !phases
                .iter()
                .any(|phase| phase.contains("Preparing complete prerequisite source"))
        );
        assert!(mock.seen.is_empty());
        assert_eq!(
            result.active_generation.as_deref(),
            Some(generation.as_str())
        );
        assert_eq!(result.generations[0].state, "Retired");
        assert_eq!(result.generations[1].state, "Ready");
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert_eq!(progress["consents"][2]["attempts"][2]["state"], "Cancelled");
        assert_eq!(progress["consents"][3]["attempts"][2]["state"], "Ambiguous");
        assert_eq!(progress["consents"][6]["attempts"][0]["state"], "Reused");
        assert_eq!(progress["consents"][6]["attempts"][1]["state"], "Declined");
        assert_eq!(progress["consents"][6]["attempts"][2]["state"], "Reused");
        assert_eq!(progress["consents"][6]["retryAuthorized"], true);
        // Optional-only plans still require explicit review, but a declined
        // prerequisite is not acquired or executed, even if its source is absent.
        plan["prerequisites"] = json!([{"id":"optional","name":"Optional","file":installer,"path":"original","arguments":[],"optional":true,"evidence":"fixture","successExitCodes":[0]}]);
        let optional_id = save(&lib, &profile, &plan);
        let next = lib
            .install_saved_plan(&profile, &optional_id, None)
            .unwrap();
        let next_id = next.generations.last().unwrap().id.clone();
        assert_eq!(next.active_generation.as_deref(), Some(generation.as_str()));
        let fp = lib
            .prerequisite_requirements(&profile, &optional_id)
            .unwrap()
            .fingerprint;
        let mut fresh = Mock {
            outcomes: VecDeque::from([PrerequisiteOutcome::Cancelled]),
            seen: vec![],
        };
        let initial = lib
            .confirm_prerequisites(&profile, &next_id, &fp, &["optional".into()], false)
            .unwrap();
        lib.process_prerequisites(&profile, &next_id, &initial, None, &mut fresh)
            .unwrap();
        assert_eq!(fresh.seen, ["optional"]);
        let consent = lib
            .confirm_prerequisites(&profile, &next_id, &fp, &["optional".into()], true)
            .unwrap();
        // Simulate a crash with a durably journaled Running attempt.
        lib.conn
            .execute(
                "UPDATE prerequisite_consents SET state='Running' WHERE id=?1",
                [&consent],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO prerequisite_attempts(id,consent_id,prerequisite_id,state) VALUES (?1,?2,'optional','Running')",params![id(),consent]).unwrap();
        let progress = lib.prerequisite_progress(&profile, &next_id).unwrap();
        assert_eq!(progress["consents"][1]["attempts"][0]["state"], "Ambiguous");
        assert!(
            lib.confirm_prerequisites(&profile, &next_id, &fp, &[], false)
                .is_err()
        );
        assert!(
            lib.process_prerequisites(&profile, &next_id, &consent, None, &mut mock)
                .is_err()
        );
        let object = lib
            .object(&format!(
                "adfile:sha256:{}",
                installer["sha256"].as_str().unwrap()
            ))
            .unwrap();
        fs::remove_file(object).unwrap();
        let consent = lib
            .confirm_prerequisites(&profile, &next_id, &fp, &[], true)
            .unwrap();
        let result = lib
            .process_prerequisites(&profile, &next_id, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(result.active_generation.as_deref(), Some(next_id.as_str()));
        assert!(mock.seen.is_empty());
        assert_eq!(
            lib.prerequisite_progress(&profile, &next_id).unwrap()["consents"][2]["attempts"][0]["state"],
            "Declined"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_prerequisite_consent_and_signed_exit_codes_fail_closed() {
        use crate::prerequisite::{PrerequisiteExecutor, PrerequisiteOutcome, PrerequisiteRequest};
        struct Outcome(i32, usize);
        impl PrerequisiteExecutor for Outcome {
            fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
                assert_eq!(fs::read(&request.source.file).unwrap(), b"inert fixture");
                self.1 += 1;
                PrerequisiteOutcome::Exited(self.0)
            }
        }
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
        let other = lib
            .add_profile("other", "http://localhost:8788", false)
            .unwrap()
            .id;
        let (app, release, revision) = (id(), id(), id());
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)", params![profile.0,release,app]).unwrap();
        let payload = source(&lib, &profile, &release, b"payload", false);
        let installer = source(&lib, &profile, &release, b"inert fixture", false);
        let plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":"windows/x86_64","selections":{},"unresolved":[],"files":{"payload.bin":payload},"directories":[],"prerequisites":[{"id":"required","name":"Inert fixture","file":installer,"path":"","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[-1]}],"launch":null});
        let plan_id = save(&lib, &profile, &plan);
        let pending = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        let generation = &pending.generations[0];
        let fp = lib
            .prerequisite_requirements(&profile, &plan_id)
            .unwrap()
            .fingerprint;
        let selected = ["required".into()];
        let stale = lib
            .confirm_prerequisites(&profile, &generation.id, &fp, &selected, false)
            .unwrap();
        let consent = lib
            .confirm_prerequisites(&profile, &generation.id, &fp, &selected, false)
            .unwrap();
        let mut outcome = Outcome(3010, 0);
        assert!(
            lib.process_prerequisites(&profile, &generation.id, &stale, None, &mut outcome)
                .is_err()
        );
        assert!(
            lib.process_prerequisites(&other, &generation.id, &consent, None, &mut outcome)
                .is_err()
        );
        let installed = generation.path.join("payload.bin");
        fs::write(&installed, b"changed").unwrap();
        assert!(
            lib.process_prerequisites(&profile, &generation.id, &consent, None, &mut outcome)
                .is_err()
        );
        assert_eq!(outcome.1, 0);
        fs::write(&installed, b"payload").unwrap();
        let rejected = lib
            .process_prerequisites(&profile, &generation.id, &consent, None, &mut outcome)
            .unwrap();
        assert!(rejected.active_generation.is_none());
        assert_eq!(outcome.1, 1);
        drop(lib);
        let mut lib = Library::open(paths).unwrap();
        let progress = lib.prerequisite_progress(&profile, &generation.id).unwrap();
        assert_eq!(progress["consents"][1]["attempts"][0]["exitCode"], 3010);
        assert_eq!(progress["consents"][1]["attempts"][0]["state"], "Failed");
        for code in [1641, 0, -1] {
            let round = lib
                .confirm_prerequisites(&profile, &generation.id, &fp, &selected, true)
                .unwrap();
            let mut outcome = Outcome(code, 0);
            let state = lib
                .process_prerequisites(&profile, &generation.id, &round, None, &mut outcome)
                .unwrap();
            assert_eq!(outcome.1, 1);
            assert_eq!(state.active_generation.is_some(), code == -1);
            let progress = lib.prerequisite_progress(&profile, &generation.id).unwrap();
            let last = progress["consents"].as_array().unwrap().last().unwrap();
            assert_eq!(last["attempts"][0]["exitCode"], code);
            assert_eq!(
                last["attempts"][0]["state"],
                if code == -1 { "Succeeded" } else { "Failed" }
            );
        }
        lib.verify_release_installation(&profile, &release).unwrap();
    }

    #[test]
    fn embedded_tree_prerequisites_share_prepared_payload() {
        exercise_tree_steps(Some(""), false, 8 * 1024 * 1024);
    }

    #[test]
    fn nested_tree_prerequisites_use_the_mapped_directory() {
        exercise_tree_steps(Some("media/"), false, 32);
    }

    #[test]
    fn separate_tree_workspace_transfers_ownership_or_rebuilds_modified_sources() {
        exercise_tree_steps(None, false, 32);
        exercise_tree_steps(None, true, 32);
    }

    fn exercise_tree_steps(prefix: Option<&str>, modify: bool, size: usize) {
        use crate::prerequisite::{PrerequisiteExecutor, PrerequisiteOutcome, PrerequisiteRequest};
        struct Recorder {
            roots: Vec<PathBuf>,
            owners: Vec<String>,
            modify: bool,
        }
        impl PrerequisiteExecutor for Recorder {
            fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
                assert_eq!(request.source.directory, request.source.root.join("setup"));
                assert!(!request.source.root.join("setup/output").exists());
                if self.modify && self.roots.is_empty() {
                    fs::write(
                        request.source.root.join("setup/output"),
                        b"installer output",
                    )
                    .unwrap();
                }
                #[cfg(windows)]
                {
                    use crate::prerequisite::NativeExecutor;
                    assert_eq!(
                        NativeExecutor.execute(request),
                        PrerequisiteOutcome::Exited(0)
                    );
                }
                self.roots.push(request.source.root.clone());
                self.owners.push(request.source.owner_id.clone());
                PrerequisiteOutcome::Exited(0)
            }
        }
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
        let (app, release, revision) = (id(), id(), id());
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
        #[cfg(windows)]
        let image = fs::read(std::env::current_exe().unwrap()).unwrap();
        #[cfg(not(windows))]
        let image = vec![42; size];
        #[cfg(windows)]
        let _ = size;
        let tree = source_layout(
            &lib,
            &profile,
            &release,
            &image,
            Some("setup/native-prerequisite-fixture.exe"),
            None,
        );
        let steps: Vec<_> = ["a","b"].into_iter().map(|id| {
            json!({"id":id,"name":id,"file":tree,"path":"setup/native-prerequisite-fixture.exe","arguments":["--exact","release_install::tests::windows_prerequisite_fixture_child"],"optional":false,"evidence":"fixture","successExitCodes":[0]})
        }).collect();
        let mut plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":crate::launch::host().unwrap(),"selections":{},"unresolved":[],"files":{"setup/native-prerequisite-fixture.exe":tree},"directories":["setup"],"prerequisites":steps,"launch":null});
        if prefix == Some("media/") {
            plan["directories"] = json!(["media", "media/setup"]);
            plan["files"] = json!({"media/setup/native-prerequisite-fixture.exe":tree,"app":source(&lib,&profile,&release,b"app",false)});
        } else if prefix.is_none() {
            plan["directories"] = json!([]);
            plan["files"] = json!({"app":source(&lib,&profile,&release,b"app",false)});
        }
        let saved = save(&lib, &profile, &plan);
        let pending = lib.install_saved_plan(&profile, &saved, None).unwrap();
        let generation = &pending.generations.last().unwrap().id;
        let fingerprint = lib
            .prerequisite_requirements(&profile, &saved)
            .unwrap()
            .fingerprint;
        let round = lib
            .confirm_prerequisites(
                &profile,
                generation,
                &fingerprint,
                &["a".into(), "b".into()],
                false,
            )
            .unwrap();
        let mut recorder = Recorder {
            roots: vec![],
            owners: vec![],
            modify,
        };
        let ready = lib
            .process_prerequisites(&profile, generation, &round, None, &mut recorder)
            .unwrap();
        assert_eq!(ready.active_generation.as_ref(), Some(generation));
        let payload = &pending.generations.last().unwrap().path;
        assert_eq!(recorder.roots.len(), 2);
        if let Some(prefix) = prefix {
            assert_eq!(recorder.roots, vec![payload.join(prefix); 2]);
            assert_eq!(recorder.owners, vec![generation.as_str(); 2]);
        } else {
            assert_ne!(recorder.roots[0], recorder.roots[1]);
            assert_eq!(recorder.owners[0] == recorder.owners[1], !modify);
            assert!(recorder.roots.iter().all(|root| !root.exists()));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_prerequisite_fixture_child() {
        let exe = std::env::current_exe().unwrap();
        if exe.file_name().unwrap() == "native-prerequisite-fixture.exe" {
            assert_eq!(
                std::fs::canonicalize(".").unwrap(),
                std::fs::canonicalize(exe.parent().unwrap()).unwrap()
            );
            assert!(std::path::Path::new("native-prerequisite-fixture.exe").is_file());
        }
    }

    #[test]
    fn prerequisite_source_layout_preserves_neighbors_and_recovers() {
        use crate::prerequisite::{PrerequisiteExecutor, PrerequisiteOutcome, PrerequisiteRequest};
        struct Mock {
            tree_outcome: PrerequisiteOutcome,
            seen: Vec<String>,
            roots: Vec<PathBuf>,
        }
        impl PrerequisiteExecutor for Mock {
            fn execute(&mut self, request: &PrerequisiteRequest<'_>) -> PrerequisiteOutcome {
                let source = request.source;
                assert_eq!(source.file.parent(), Some(source.directory.as_path()));
                assert!(source.file.starts_with(&source.root));
                assert!(!source.root.starts_with(request.payload));
                assert!(!request.payload.join("setup").exists());
                self.seen.push(request.prerequisite.id.clone());
                self.roots.push(source.root.clone());
                if request.prerequisite.id == "file" {
                    assert_eq!(source.directory, source.root);
                    assert_eq!(source.file, source.root.join("prerequisite"));
                    assert_eq!(fs::read(&source.file).unwrap(), b"standalone");
                    fs::write(&source.file, b"modified independent File").unwrap();
                    PrerequisiteOutcome::Exited(0)
                } else {
                    assert_eq!(source.file, source.root.join("setup/installer.exe"));
                    assert_eq!(source.directory, source.root.join("setup"));
                    assert_eq!(fs::read(&source.file).unwrap(), b"installer");
                    assert_eq!(
                        fs::read(source.directory.join("support.dat")).unwrap(),
                        b"neighbor"
                    );
                    assert!(source.root.join("empty").is_dir());
                    fs::write(source.directory.join("support.dat"), b"changed neighbor").unwrap();
                    fs::write(source.directory.join("generated.log"), b"mock output").unwrap();
                    self.tree_outcome.clone()
                }
            }
        }
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
        let (app, release, revision) = (id(), id(), id());
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                params![profile.0, app],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
        let payload = source(&lib, &profile, &release, b"application", false);
        let file = source(&lib, &profile, &release, b"standalone", false);
        let tree = source_layout(
            &lib,
            &profile,
            &release,
            b"installer",
            Some("setup/installer.exe"),
            Some(("setup/support.dat", b"neighbor")),
        );
        let mut plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":crate::launch::host().unwrap(),"selections":{},"unresolved":[],"files":{"app":payload},"directories":[],"prerequisites":[],"launch":null});
        let old_plan = save(&lib, &profile, &plan);
        let old = lib
            .install_saved_plan(&profile, &old_plan, None)
            .unwrap()
            .active_generation
            .unwrap();
        plan["prerequisites"] = json!([
            {"id":"file","name":"Standalone","file":file,"path":"","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]},
            {"id":"tree","name":"Tree installer","file":tree,"path":"setup/installer.exe","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]}
        ]);
        let plan_id = save(&lib, &profile, &plan);
        let pending = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        let generation = pending.generations.last().unwrap().id.clone();
        let fp = lib
            .prerequisite_requirements(&profile, &plan_id)
            .unwrap()
            .fingerprint;
        let selection = vec!["file".into(), "tree".into()];
        let neighbor_digest = format!("{:x}", Sha256::digest(b"neighbor"));
        let neighbor = lib
            .object(&format!("adfile:sha256:{neighbor_digest}"))
            .unwrap();
        let standalone = lib.object(file["artifactId"].as_str().unwrap()).unwrap();
        let mut mock = Mock {
            tree_outcome: PrerequisiteOutcome::Ambiguous("mock interrupted".into()),
            seen: vec![],
            roots: vec![],
        };
        // Supporting file corruption must fail before that executor is called.
        fs::write(&neighbor, b"corrupt!").unwrap();
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fp, &selection, false)
            .unwrap();
        let result = lib
            .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(result.active_generation.as_deref(), Some(old.as_str()));
        assert_eq!(mock.seen, ["file"]);
        assert!(!mock.roots[0].exists());
        assert_eq!(fs::read(&standalone).unwrap(), b"standalone");
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert_eq!(progress["consents"][0]["attempts"][1]["state"], "Failed");
        assert!(progress["consents"][0]["attempts"][1]["sourceWorkspace"].is_null());
        fs::write(&neighbor, b"neighbor").unwrap();
        #[cfg(target_os = "linux")]
        {
            assert_eq!(lib.collect_unreferenced_objects().unwrap(), 0);
            assert!(
                lib.remove_local_artifact(&profile, tree["associationId"].as_str().unwrap())
                    .is_err()
            );
        }

        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fp, &selection, true)
            .unwrap();
        let result = lib
            .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(result.active_generation.as_deref(), Some(old.as_str()));
        assert_eq!(
            mock.seen,
            ["file", "tree"],
            "{:?}",
            result.generations.last().unwrap().last_error
        );
        let ambiguous_root = mock.roots.last().unwrap().clone();
        assert_eq!(
            fs::read(ambiguous_root.join("setup/support.dat")).unwrap(),
            b"changed neighbor"
        );
        assert_eq!(fs::read(&neighbor).unwrap(), b"neighbor");
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert_eq!(progress["consents"][1]["attempts"][1]["state"], "Ambiguous");
        assert!(progress["consents"][1]["attempts"][1]["sourceWorkspace"].is_string());

        // Reopening and explicitly retrying uses a new unmodified source.
        drop(lib);
        let mut lib = Library::open(paths).unwrap();
        mock.tree_outcome = PrerequisiteOutcome::Exited(0);
        let consent = lib
            .confirm_prerequisites(&profile, &generation, &fp, &selection, true)
            .unwrap();
        let result = lib
            .process_prerequisites(&profile, &generation, &consent, None, &mut mock)
            .unwrap();
        assert_eq!(
            result.active_generation.as_deref(),
            Some(generation.as_str())
        );
        assert_ne!(mock.roots.last().unwrap(), &ambiguous_root);
        assert!(!mock.roots.last().unwrap().exists());
        assert!(ambiguous_root.exists());
        assert_eq!(fs::read(&neighbor).unwrap(), b"neighbor");
        assert_eq!(fs::read(&standalone).unwrap(), b"standalone");
        lib.verify_release_installation(&profile, &release).unwrap();
        // Simulate a crash after preparation but before a definite outcome's
        // workspace was cleaned. Progress must recover its owned partial tree.
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        let attempt = progress["consents"][2]["attempts"][1]["id"]
            .as_str()
            .unwrap();
        let workspace =
            crate::materialization::prerequisite_workspace(&lib.paths.content, attempt).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join(".app-depot-prerequisite-id"), attempt).unwrap();
        fs::create_dir(workspace.join("partial")).unwrap();
        fs::write(workspace.join("partial/file"), b"interrupted copy").unwrap();
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert!(progress["consents"][2]["attempts"][1]["sourceCleanupError"].is_null());
        assert!(!workspace.exists());
        // A foreign directory at an attempt-derived path must not be erased.
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join(".app-depot-prerequisite-id"), id()).unwrap();
        fs::write(workspace.join("foreign"), b"preserve").unwrap();
        let progress = lib.prerequisite_progress(&profile, &generation).unwrap();
        assert!(progress["consents"][2]["attempts"][1]["sourceCleanupError"].is_string());
        assert_eq!(fs::read(workspace.join("foreign")).unwrap(), b"preserve");
        // Dispose of the synthetic foreign data only within this test fixture.
        fs::remove_dir_all(&workspace).unwrap();
        #[cfg(target_os = "linux")]
        {
            lib.uninstall_release_installation(&profile, &release)
                .unwrap();
            assert!(!ambiguous_root.exists());
            assert_eq!(fs::read(&neighbor).unwrap(), b"neighbor");
        }
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
        let image = fs::read(std::env::current_exe().unwrap()).unwrap();
        let f = source(&lib, &profile, &release, &image, false);
        let t = source(&lib, &profile, &release, b"fixture Tree", true);
        let association = f["associationId"].as_str().unwrap().to_owned();
        let plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":"windows/x86_64","selections":{},"unresolved":[],"files":{"renamed":f,"data/renamed":t},"directories":["data","empty"],"prerequisites":[],"launch":{"method":"NativePortableV1","associationId":association,"entryPointId":"fixture","revisionId":revision,"path":"renamed","workingDirectory":"","arguments":[]}});
        let plan_id = save(&lib, &profile, &plan);
        let first = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        assert_eq!(first.generations[0].state, "Ready");
        lib.verify_release_installation(&profile, &release).unwrap();
        assert!(first.generations[0].path.join("empty").is_dir());
        let first_id = first.active_generation.unwrap();
        let candidate = lib.launch_candidates(&profile, &release).unwrap().remove(0);
        assert_eq!(candidate.status, "Ready");
        assert_eq!(candidate.selection_revision(), first_id);
        assert!(
            lib.play_declared(&profile, &release, &association, "fixture", &revision)
                .is_err()
        );
        lib.conn
            .execute(
                "UPDATE installation_generations SET process_start='windows:pending' WHERE id=?1",
                [&first_id],
            )
            .unwrap();
        assert_eq!(
            lib.launch_candidates(&profile, &release).unwrap()[0].status,
            "ProcessUnknown"
        );
        assert!(
            lib.windows_active_processes()
                .unwrap()
                .iter()
                .any(|(id, state)| id == &first_id
                    && *state == crate::windows_process::ProcessState::Unknown)
        );
        assert!(
            lib.play_declared(&profile, &release, &association, "fixture", &first_id)
                .is_err()
        );
        assert!(
            lib.uninstall_release_installation(&profile, &release)
                .is_err()
        );
        lib.conn
            .execute(
                "UPDATE installation_generations SET process_start=NULL WHERE id=?1",
                [&first_id],
            )
            .unwrap();
        let mut blocked = plan.clone();
        blocked["prerequisites"] = json!([{"id":"required","name":"Inert prerequisite","file":t,"path":"original","arguments":[],"optional":false,"evidence":"fixture","successExitCodes":[0]}]);
        let blocked_id = save(&lib, &profile, &blocked);
        let next = lib.install_saved_plan(&profile, &blocked_id, None).unwrap();
        assert_eq!(next.generations[0].state, "Ready");
        assert_eq!(next.generations[1].state, "PrerequisitesPending");
        lib.verify_release_installation(&profile, &release).unwrap();
        let candidate = lib.launch_candidates(&profile, &release).unwrap().remove(0);
        assert_eq!(candidate.status, "Ready");
        assert_eq!(candidate.selection_revision(), first_id);
        let extra = next.generations[0].path.join("foreign.txt");
        fs::write(&extra, "preserve").unwrap();
        assert!(
            lib.uninstall_release_installation(&profile, &release)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&extra).unwrap(), "preserve");
        fs::remove_file(&extra).unwrap();
        let pending_id = &next.generations[1].id;
        let fingerprint = lib
            .prerequisite_requirements(&profile, &blocked_id)
            .unwrap()
            .fingerprint;
        let consent = lib
            .confirm_prerequisites(
                &profile,
                pending_id,
                &fingerprint,
                &["required".into()],
                false,
            )
            .unwrap();
        let attempt = id();
        lib.conn.execute("INSERT INTO prerequisite_attempts(id,consent_id,prerequisite_id,state) VALUES (?1,?2,'required','Failed')", params![attempt,consent]).unwrap();
        let workspace =
            crate::materialization::prerequisite_workspace(&lib.paths.content, &attempt).unwrap();
        fs::create_dir(&workspace).unwrap();
        let foreign = workspace.join("foreign.txt");
        fs::write(&foreign, "preserve").unwrap();
        assert!(
            lib.uninstall_release_installation(&profile, &release)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "preserve");
        fs::remove_file(&foreign).unwrap();
        fs::remove_dir(&workspace).unwrap();
        lib.uninstall_release_installation(&profile, &release)
            .unwrap();
        assert!(
            lib.release_installation(&profile, &release)
                .unwrap()
                .is_none()
        );
        for generation in &next.generations {
            assert!(!generation.path.parent().unwrap().exists());
        }
        lib.install_saved_plan(&profile, &plan_id, None).unwrap();
        lib.verify_release_installation(&profile, &release).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_generation_removal_recovers_after_process_interruption() {
        const ROOT: &str = "APP_DEPOT_GENERATION_REMOVAL_TEST_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = std::path::PathBuf::from(root);
            let mut lib = Library::open(LibraryPaths {
                metadata: root.join("metadata"),
                content: root.join("content"),
            })
            .unwrap();
            let profile =
                ServerProfileId(std::env::var("APP_DEPOT_GENERATION_TEST_PROFILE").unwrap());
            let release = std::env::var("APP_DEPOT_GENERATION_TEST_RELEASE").unwrap();
            lib.uninstall_release_installation(&profile, &release)
                .unwrap();
            panic!("expected interruption boundary");
        }
        for boundary in ["after-intent", "after-rename", "after-cleanup"] {
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
            lib.conn
                .execute(
                    "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,'{}',0)",
                    params![profile.0, app],
                )
                .unwrap();
            lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,'{}',0)",params![profile.0,release,app]).unwrap();
            let f = source(&lib, &profile, &release, b"file", false);
            let t = source(&lib, &profile, &release, b"tree", true);
            let plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision,"platform":"windows/x86_64","selections":{},"unresolved":[],"files":{"data/file":f,"tree":t},"directories":["data","empty"],"prerequisites":[],"launch":null});
            let plan_id = save(&lib, &profile, &plan);
            let installed = lib.install_saved_plan(&profile, &plan_id, None).unwrap();
            let root = installed.generations[0].path.parent().unwrap().to_owned();
            let unrelated = paths.content.join("apps").join("unrelated.txt");
            fs::write(&unrelated, "preserve").unwrap();
            drop(lib);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","release_install::tests::windows_generation_removal_recovers_after_process_interruption"])
                .env(ROOT,temp.path())
                .env("APP_DEPOT_GENERATION_TEST_PROFILE",&profile.0)
                .env("APP_DEPOT_GENERATION_TEST_RELEASE",&release)
                .env("APP_DEPOT_GENERATION_REMOVAL_BOUNDARY",boundary)
                .spawn().unwrap();
            let marker = paths
                .content
                .join("apps")
                .join(".generation-removal-test-ready");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while !marker.exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before {boundary}"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "timeout at {boundary}"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            child.kill().unwrap();
            child.wait().unwrap();
            fs::remove_file(&marker).unwrap();
            let mut recovered = Library::open(paths.clone()).unwrap();
            assert!(
                recovered
                    .release_installation(&profile, &release)
                    .unwrap()
                    .is_none(),
                "{boundary}"
            );
            assert!(!root.exists());
            assert_eq!(fs::read_to_string(&unrelated).unwrap(), "preserve");
            for data in [b"file".as_slice(), b"tree".as_slice()] {
                let hash = format!("{:x}", Sha256::digest(data));
                assert_eq!(
                    fs::read(recovered.object(&format!("adfile:sha256:{hash}")).unwrap()).unwrap(),
                    data
                );
            }
            recovered
                .uninstall_release_installation(&profile, &release)
                .unwrap();
            recovered
                .install_saved_plan(&profile, &plan_id, None)
                .unwrap();
            recovered
                .verify_release_installation(&profile, &release)
                .unwrap();
        }
    }
}
