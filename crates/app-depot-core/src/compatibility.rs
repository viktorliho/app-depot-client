//! Existing host runtimes for Windows applications. Discovery only reads Steam metadata.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone)]
pub enum Runtime {
    Wine(PathBuf),
    Proton {
        launcher: PathBuf,
        container: PathBuf,
        steam: PathBuf,
    },
}

impl Runtime {
    pub fn executable(&self) -> &Path {
        match self {
            Self::Wine(path) => path,
            Self::Proton { launcher, .. } => launcher,
        }
    }

    /// Initialize a library-owned prefix without invoking Wine's optional
    /// Gecko or Mono installers. Existing prefixes retain installed components.
    pub fn prepare_prefix(
        &self,
        library: &crate::library::LibraryPaths,
        profile: &crate::ServerProfileId,
        app: &str,
    ) -> Result<PathBuf, crate::library::LibraryError> {
        use crate::library::{LibraryError, ensure_directory};
        let app = uuid::Uuid::parse_str(app)
            .map_err(|_| LibraryError::Invalid("Invalid application ID".into()))?;
        let profile = uuid::Uuid::parse_str(&profile.0)
            .map_err(|_| LibraryError::Invalid("Invalid profile ID".into()))?;
        let data = library
            .content
            .join("prefixes")
            .join(profile.to_string())
            .join(app.to_string());
        ensure_directory(&data)?;
        let prefix = data.join("pfx");
        ensure_directory(&prefix)?;
        let status = self
            .command(
                &data,
                Path::new(r"C:\windows\system32\wineboot.exe"),
                &data,
                &["-u".into()],
            )
            .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
            .status()?;
        if !status.success() {
            return Err(LibraryError::Invalid(format!(
                "Prefix initialization failed ({status})"
            )));
        }
        Ok(data)
    }

    /// The caller supplies a private, persistent application directory.
    pub fn command(&self, data: &Path, target: &Path, cwd: &Path, args: &[String]) -> Command {
        let mut command = match self {
            Self::Wine(wine) => {
                let mut cmd = Command::new(wine);
                cmd.env("WINEPREFIX", data.join("pfx"));
                cmd
            }
            Self::Proton {
                launcher,
                container,
                steam,
            } => {
                let mut cmd = Command::new(container);
                cmd.args(["--verb=waitforexitandrun", "--"])
                    .arg(launcher)
                    .arg("waitforexitandrun")
                    .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam)
                    .env("STEAM_COMPAT_DATA_PATH", data)
                    .env_remove("WINEPREFIX");
                cmd
            }
        };
        if let Some(host_path) = env::var_os("APP_DEPOT_HOST_LD_LIBRARY_PATH") {
            if host_path.is_empty() {
                command.env_remove("LD_LIBRARY_PATH");
            } else {
                command.env("LD_LIBRARY_PATH", host_path);
            }
        }
        for (key, _) in env::vars_os() {
            if crate::launch::sensitive_env_name(&key) {
                command.env_remove(key);
            }
        }
        command.arg(target).args(args).current_dir(cwd);
        command
    }
}

fn values(file: &Path, key: &str) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().strip_prefix('"')?.split_once('"')?;
            let value = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
            (name == key).then(|| value.replace("\\\\", "\\"))
        })
        .collect()
}

pub fn discover() -> Vec<Runtime> {
    let mut runtimes = Vec::new();
    let home = env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home {
        let data = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        for directory in [data.join("lutris/runners/wine"), PathBuf::from("/opt")] {
            runtimes.extend(
                fs::read_dir(directory)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path().join("bin/wine"))
                    .filter(|p| p.is_file())
                    .map(Runtime::Wine),
            );
        }
        if let Some(steam) = [data.join("Steam"), home.join(".steam/steam")]
            .into_iter()
            .find(|root| root.join("steamapps").is_dir())
        {
            let mut libraries = vec![steam.join("steamapps")];
            libraries.extend(
                values(&steam.join("steamapps/libraryfolders.vdf"), "path")
                    .into_iter()
                    .map(|p| PathBuf::from(p).join("steamapps")),
            );
            let dirs = libraries
                .iter()
                .map(|lib| lib.join("common"))
                .chain([steam.join("compatibilitytools.d")]);
            for entry in dirs.flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten()) {
                let root = entry.path();
                let launcher = root.join("proton");
                if !launcher.is_file() {
                    continue;
                }
                let Some(id) = values(&root.join("toolmanifest.vdf"), "require_tool_appid")
                    .into_iter()
                    .next()
                else {
                    continue;
                };
                if !id.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                for lib in &libraries {
                    let manifest = lib.join(format!("appmanifest_{id}.acf"));
                    if let Some(container) = values(&manifest, "installdir")
                        .into_iter()
                        .filter(|s| Path::new(s).file_name() == Some(s.as_ref()))
                        .map(|s| lib.join("common").join(s).join("_v2-entry-point"))
                        .find(|p| p.is_file())
                    {
                        runtimes.push(Runtime::Proton {
                            launcher: launcher.clone(),
                            container,
                            steam: steam.clone(),
                        });
                        break;
                    }
                }
            }
        }
    }
    if let Some(path) = env::var_os("PATH") {
        runtimes.extend(
            env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("wine"))
                .filter_map(|p| p.canonicalize().ok())
                .filter(|p| p.is_file())
                .map(Runtime::Wine),
        );
    }
    runtimes.sort_by(|a, b| a.executable().cmp(b.executable()));
    runtimes.dedup_by(|a, b| a.executable() == b.executable());
    runtimes
}

pub struct ApplicationRuntime {
    pub runtime: Runtime,
    pub data: PathBuf,
}

impl crate::library::Library {
    fn binding_key(profile: &crate::ServerProfileId, app: &str) -> String {
        format!("compatibility/{}/{app}", profile.0)
    }

    pub fn application_runtime(
        &self,
        profile: &crate::ServerProfileId,
        app: &str,
    ) -> Result<ApplicationRuntime, crate::library::LibraryError> {
        use crate::library::LibraryError;
        use rusqlite::OptionalExtension;
        let key = Self::binding_key(profile, app);
        let path: String = self
            .conn
            .query_row("SELECT value FROM library_meta WHERE key=?1", [&key], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| {
                LibraryError::Invalid("Application has no selected compatibility runtime".into())
            })?;
        let runtime = discover()
            .into_iter()
            .find(|r| r.executable() == Path::new(&path))
            .ok_or_else(|| {
                LibraryError::Invalid(format!("Installed runtime is unavailable: {path}"))
            })?;
        let data = self
            .paths
            .content
            .join("prefixes")
            .join(&profile.0)
            .join(app);

        Ok(ApplicationRuntime { runtime, data })
    }

    pub fn cancel_compatibility_prerequisite(
        &self,
        profile: &crate::ServerProfileId,
        release: &str,
    ) -> Result<(), crate::library::LibraryError> {
        use crate::library::LibraryError;
        let (app, attempt):(String,String)=self.conn.query_row("SELECT i.app_id,a.id FROM prerequisite_attempts a JOIN prerequisite_consents c ON c.id=a.consent_id JOIN installation_generations g ON g.id=c.generation_id JOIN release_installations i ON i.id=g.installation_id WHERE i.profile_id=?1 AND i.release_id=?2 AND a.state='Running' AND c.state='Running'",rusqlite::params![profile.0,release],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let runtime = self.application_runtime(profile, &app)?;
        let attempt = uuid::Uuid::parse_str(&attempt)
            .map_err(|_| LibraryError::Invalid("Invalid prerequisite attempt".into()))?;
        let work = runtime.data.join(format!("attempt-{attempt}"));
        crate::launch::open_nofollow(&work, "", true)?;
        match fs::File::create_new(work.join("cancel")) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub fn bind_application_runtime(
        &self,
        profile: &crate::ServerProfileId,
        plan_id: &str,
        selected: &Path,
    ) -> Result<ApplicationRuntime, crate::library::LibraryError> {
        use crate::library::LibraryError;
        let _lock = self.manifest_lifecycle_lock()?;
        let plan = self.load_install_plan(profile, plan_id)?;
        if plan.platform != "windows/x86_64" {
            return Err(LibraryError::Invalid(
                "Expected Windows installation plan".into(),
            ));
        }
        let key = Self::binding_key(profile, &plan.app_id);
        let selected = selected
            .to_str()
            .ok_or_else(|| LibraryError::Invalid("Invalid runtime path".into()))?;
        if !discover()
            .iter()
            .any(|r| r.executable() == Path::new(selected))
        {
            return Err(LibraryError::Invalid(format!(
                "Selected runtime is unavailable: {selected}"
            )));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO library_meta(key,value) VALUES(?1,?2)",
            rusqlite::params![key, selected],
        )?;
        self.application_runtime(profile, &plan.app_id)
    }
}

impl crate::prerequisite::PrerequisiteExecutor for ApplicationRuntime {
    fn execute(
        &mut self,
        request: &crate::prerequisite::PrerequisiteRequest<'_>,
    ) -> crate::prerequisite::PrerequisiteOutcome {
        use crate::prerequisite::PrerequisiteOutcome;
        let run = || -> std::io::Result<_> {
            use std::io::Write;
            crate::launch::open_nofollow(&self.data, "pfx", true).map_err(std::io::Error::other)?;
            let work = self.data.join(format!(
                "attempt-{}",
                uuid::Uuid::parse_str(request.attempt_id).map_err(std::io::Error::other)?
            ));
            fs::create_dir(&work)?;
            let helper = work.join("supervisor.exe");
            let result = work.join("result");
            let windows = |path: &Path| -> std::io::Result<String> {
                let path = path
                    .to_str()
                    .filter(|p| p.starts_with('/') && !p.contains('\\'))
                    .ok_or_else(|| std::io::Error::other("Path cannot be represented in Wine"))?;
                Ok(format!("Z:{}", path.replace('/', "\\")))
            };
            let mut file = fs::File::create_new(&helper)?;
            file.write_all(include_bytes!(concat!(env!("OUT_DIR"), "/supervisor.exe")))?;
            file.sync_all()?;
            drop(file);
            let target = windows(&request.source.file)?;
            let arguments = std::iter::once(target.clone())
                .chain(request.prerequisite.arguments.iter().cloned())
                .collect::<Vec<_>>();
            let status = self
                .runtime
                .command(&self.data, &helper, &request.source.directory, &arguments)
                .env("DEPOT_INSTALLER", target)
                .env("DEPOT_RESULT", windows(&result)?)
                .env("DEPOT_CANCEL", windows(&work.join("cancel"))?)
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "Installer supervisor returned {status}"
                )));
            }
            let bytes = fs::read(&result)?;
            let code = i32::from_le_bytes(
                bytes
                    .try_into()
                    .map_err(|_| std::io::Error::other("Incomplete installer outcome"))?,
            );
            // Cleanup does not change an already confirmed installer outcome.
            let cancelled = work.join("cancel").is_file() && code == 1602;
            let _ = fs::remove_file(work.join("cancel"));
            let _ = fs::remove_file(result);
            let _ = fs::remove_file(helper);
            let _ = fs::remove_dir(work);
            Ok((code, cancelled))
        };
        match run() {
            Ok((_, true)) => PrerequisiteOutcome::Cancelled,
            Ok((1602, false)) if !request.prerequisite.success_exit_codes.contains(&1602) => {
                PrerequisiteOutcome::Cancelled
            }
            Ok((code, false)) => PrerequisiteOutcome::Exited(code),
            Err(e) => PrerequisiteOutcome::Ambiguous(e.to_string()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires installed host Wine and a graphical session"]
    fn installed_wine_uses_private_prefix_and_literal_arguments() {
        let runtime = discover()
            .into_iter()
            .find(|r| matches!(r, Runtime::Wine(_)))
            .expect("system Wine");
        let root = tempfile::tempdir().unwrap();
        let library = crate::library::LibraryPaths {
            metadata: root.path().join("metadata"),
            content: root.path().join("content"),
        };
        let data = runtime
            .prepare_prefix(
                &library,
                &crate::ServerProfileId(uuid::Uuid::new_v4().to_string()),
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        let status = runtime
            .command(
                data.as_path(),
                Path::new(r"C:\windows\system32\cmd.exe"),
                data.as_path(),
                &["/c".into(), "echo M55>C:\\proof.txt".into()],
            )
            .status()
            .unwrap();
        assert!(status.success());
        let proof = fs::read_to_string(data.as_path().join("pfx/drive_c/proof.txt")).unwrap();
        assert_eq!(proof.trim(), "M55");
    }
}
