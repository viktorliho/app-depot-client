//! Read-only NativePortableV1 projection and Linux direct process execution.
//! A declaration never grants permission to execute; callers must request an explicit Play.
use crate::{
    ServerProfileId,
    library::{Library, LibraryError},
    models::InventoryEntry,
    transfer::VerifiedFile,
    tree_download::VerifiedTree,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchCandidate {
    pub profile_id: ServerProfileId,
    pub release_id: String,
    pub association_id: String,
    pub artifact_id: String,
    pub entry_id: String,
    pub declaration_revision: String,
    pub label: Option<String>,
    pub target: Option<String>,
    pub working_directory: Option<String>,
    pub arguments: Vec<String>,
    pub status: String,
    pub reason: String,
    #[serde(skip)]
    local_root: Option<PathBuf>,
    #[serde(skip)]
    expected_hash: Option<String>,
    #[serde(skip)]
    expected_size: Option<u64>,
}
fn invalid(message: &str) -> LibraryError {
    LibraryError::Invalid(message.into())
}
fn issue(candidate: &mut LaunchCandidate, status: &str, reason: &str) {
    candidate.status = status.into();
    candidate.reason = reason.into();
}
const PLATFORMS: [&str; 6] = [
    "linux/x86_64",
    "linux/aarch64",
    "windows/x86_64",
    "windows/aarch64",
    "macos/x86_64",
    "macos/aarch64",
];
fn valid_platforms(value: &Value) -> Option<Vec<&str>> {
    let list = value.as_array()?;
    if list.len() > 32 {
        return None;
    }
    let mut found = std::collections::HashSet::new();
    list.iter()
        .map(|item| {
            let s = item.as_str()?;
            (!s.is_empty()
                && s.chars().count() <= 80
                && !s.chars().any(char::is_control)
                && found.insert(s))
            .then_some(s)
        })
        .collect()
}
fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && !path
            .as_bytes()
            .get(0..2)
            .is_some_and(|b| b[0].is_ascii_alphabetic() && b[1] == b':')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.chars().any(char::is_control)
        })
}
fn host() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux/x86_64"),
        ("linux", "aarch64") => Some("linux/aarch64"),
        _ => None,
    }
}
fn arguments(value: Option<&Value>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let list = value.as_array()?;
    if list.len() > 64 {
        return None;
    }
    let mut total = 0usize;
    list.iter()
        .map(|v| {
            let s = v.as_str()?;
            total = total.checked_add(s.len())?;
            (s.len() <= 4096 && total <= 16384 && !s.chars().any(char::is_control))
                .then(|| s.to_owned())
        })
        .collect()
}
#[cfg(target_os = "linux")]
fn sensitive_env_name(key: &std::ffi::OsStr) -> bool {
    let name = key.to_string_lossy().to_ascii_uppercase();
    name.starts_with("APP_DEPOT_")
        || name.contains("TOKEN")
        || name.contains("SECRET")
        || name.contains("PASSWORD")
        || name == "SSH_AUTH_SOCK"
}
impl Library {
    /// Assess every entry in a cached, consistent Release declaration snapshot.
    /// Assessment is local and does not contact the Server or execute anything.
    pub fn launch_candidates(
        &self,
        profile: &ServerProfileId,
        release: &str,
    ) -> Result<Vec<LaunchCandidate>, LibraryError> {
        let files = self.verified_files(Some(profile))?;
        let trees = self.verified_trees(Some(profile))?;
        self.launch_candidates_with_verified(profile, release, &files, &trees)
    }
    pub fn launch_candidates_with_verified(
        &self,
        profile: &ServerProfileId,
        release: &str,
        file_items: &[VerifiedFile],
        tree_items: &[VerifiedTree],
    ) -> Result<Vec<LaunchCandidate>, LibraryError> {
        let snapshot = self
            .cached_snapshot(profile, release)?
            .ok_or_else(|| invalid("cached Release declaration snapshot unavailable"))?
            .value;
        let release_platforms = snapshot
            .release
            .app_depot()
            .and_then(|v| v.get("platforms"));
        let associations = self.cached_associations(profile, release)?;
        let mut out = Vec::new();
        for association in associations {
            let a = association.value;
            let Some(doc) = snapshot
                .associations
                .iter()
                .find(|d| d.association_id == a.id)
            else {
                continue;
            };
            let Some(entries) = doc
                .declaration
                .app_depot()
                .and_then(|v| v.get("entryPoints"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            if entries.len() > 100 {
                continue;
            }
            let mut ids = std::collections::HashSet::new();
            for entry in entries {
                let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
                let mut c = LaunchCandidate {
                    profile_id: profile.clone(),
                    release_id: release.into(),
                    association_id: a.id.clone(),
                    artifact_id: a.artifact_id.clone(),
                    entry_id: id.into(),
                    declaration_revision: doc.declaration.revision_id.clone(),
                    label: entry
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    target: None,
                    working_directory: None,
                    arguments: Vec::new(),
                    status: "Unavailable".into(),
                    reason: String::new(),
                    local_root: None,
                    expected_hash: None,
                    expected_size: None,
                };
                if id.is_empty()
                    || id.len() > 80
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || !ids.insert(id)
                {
                    issue(&mut c, "Malformed", "Invalid or duplicate entry-point ID");
                    out.push(c);
                    continue;
                }
                let Some(launch) = entry.get("launch") else {
                    issue(
                        &mut c,
                        "NoLaunch",
                        "File reference without launch declaration",
                    );
                    out.push(c);
                    continue;
                };
                let Some(launch) = launch.as_object() else {
                    issue(&mut c, "Malformed", "Launch must be an object");
                    out.push(c);
                    continue;
                };
                let Some(method) = launch.get("method").and_then(Value::as_str) else {
                    issue(&mut c, "Malformed", "Launch method is missing or malformed");
                    out.push(c);
                    continue;
                };
                if method != "NativePortableV1" {
                    issue(
                        &mut c,
                        "UnsupportedMethod",
                        "Unknown launch method or version",
                    );
                    out.push(c);
                    continue;
                }
                let Some(entry_platforms) = entry
                    .get("platforms")
                    .and_then(valid_platforms)
                    .filter(|v| !v.is_empty())
                else {
                    issue(
                        &mut c,
                        "Malformed",
                        "Missing or malformed entry-point platforms",
                    );
                    out.push(c);
                    continue;
                };
                let release_list = match release_platforms {
                    Some(v) => match valid_platforms(v) {
                        Some(v) => Some(v),
                        None => {
                            issue(&mut c, "Malformed", "Malformed Release platforms");
                            out.push(c);
                            continue;
                        }
                    },
                    None => None,
                };
                let effective: Vec<_> = PLATFORMS
                    .iter()
                    .filter(|p| {
                        entry_platforms.contains(p)
                            && release_list
                                .as_ref()
                                .is_none_or(|v| v.is_empty() || v.contains(p))
                    })
                    .collect();
                if effective.is_empty() {
                    issue(
                        &mut c,
                        "UnsupportedPlatform",
                        "No known applicable platform",
                    );
                    out.push(c);
                    continue;
                }
                if !host().is_some_and(|h| effective.contains(&&h)) {
                    issue(
                        &mut c,
                        "UnsupportedPlatform",
                        "Host does not match declared platform",
                    );
                    out.push(c);
                    continue;
                }
                let Some(args) = arguments(launch.get("arguments")) else {
                    issue(&mut c, "Malformed", "Invalid literal process arguments");
                    out.push(c);
                    continue;
                };
                c.arguments = args;
                let Some(target) = entry.get("target").and_then(Value::as_object) else {
                    issue(&mut c, "Malformed", "Missing target");
                    out.push(c);
                    continue;
                };
                let file = target.get("type").and_then(Value::as_str) == Some("ArtifactFile");
                let tree = target.get("type").and_then(Value::as_str) == Some("TreeFile");
                if !file && !tree {
                    issue(&mut c, "Malformed", "Unsupported target type");
                    out.push(c);
                    continue;
                }
                let Some(artifact) = self.cached_artifact(profile, &a.artifact_id)? else {
                    issue(&mut c, "Unavailable", "Artifact metadata unavailable");
                    out.push(c);
                    continue;
                };
                if (file && artifact.value.kind != "File")
                    || (tree && artifact.value.kind != "Tree")
                {
                    issue(&mut c, "Malformed", "Target type does not match Artifact");
                    out.push(c);
                    continue;
                }
                let target_path = if file {
                    String::new()
                } else {
                    target
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                if tree && !path_valid(&target_path) {
                    issue(&mut c, "Malformed", "Unsafe Tree target path");
                    out.push(c);
                    continue;
                }
                let work = match launch.get("workingDirectory") {
                    Some(Value::String(v)) => v.clone(),
                    Some(_) => {
                        issue(&mut c, "Malformed", "Invalid working directory");
                        out.push(c);
                        continue;
                    }
                    None => {
                        if file {
                            String::new()
                        } else {
                            target_path.rsplit_once('/').map_or("", |(p, _)| p).into()
                        }
                    }
                };
                if !work.is_empty() && (!tree || !path_valid(&work)) {
                    issue(&mut c, "Malformed", "Unsafe working directory");
                    out.push(c);
                    continue;
                }
                c.target = Some(target_path.clone());
                c.working_directory = Some(work.clone());
                if file {
                    if let Some(v) = file_items
                        .iter()
                        .find(|v| v.association_id == a.id && v.artifact_id == a.artifact_id)
                    {
                        c.local_root = v.path.parent().map(Path::to_path_buf);
                        c.expected_size = Some(v.size);
                        c.expected_hash = a
                            .artifact_id
                            .strip_prefix("adfile:sha256:")
                            .map(str::to_owned);
                        c.target = v.path.file_name().map(|n| n.to_string_lossy().into_owned());
                    }
                } else if let Some(v) = tree_items
                    .iter()
                    .find(|v| v.association_id == a.id && v.artifact_id == a.artifact_id)
                {
                    let job_id:String=self.conn.query_row("SELECT id FROM download_jobs WHERE profile_id=?1 AND association_id=?2 AND artifact_id=?3 AND state='Completed' ORDER BY updated_at DESC LIMIT 1",rusqlite::params![profile.0,a.id,a.artifact_id],|r|r.get(0))?;
                    let inventory = self
                        .tree_inventory(&job_id)?
                        .ok_or_else(|| invalid("verified Tree inventory unavailable"))?;
                    if let Some(InventoryEntry::File { size, sha256, .. }) =
                        inventory.iter().find(|e| e.path() == target_path)
                    {
                        if work.is_empty()
                            || inventory
                                .iter()
                                .any(|e| matches!(e,InventoryEntry::Directory{path} if path==&work))
                        {
                            c.local_root = Some(v.path.clone());
                            c.expected_size = Some(*size);
                            c.expected_hash = Some(sha256.clone());
                        } else {
                            issue(
                                &mut c,
                                "Malformed",
                                "Working directory absent from verified inventory",
                            );
                        }
                    } else {
                        issue(
                            &mut c,
                            "Malformed",
                            "Declared target absent or not a file in verified inventory",
                        );
                    }
                }
                if c.local_root.is_some() {
                    #[cfg(target_os = "linux")]
                    match c.local_readiness() {
                        Ok(()) => issue(
                            &mut c,
                            "Ready",
                            "Verified content and declaration available; target hash checked again before launch",
                        ),
                        Err(reason) => issue(&mut c, "LocalUnavailable", &reason),
                    }
                    #[cfg(not(target_os = "linux"))]
                    issue(
                        &mut c,
                        "UnsupportedPlatform",
                        "Native portable execution is implemented only on Linux",
                    );
                } else if c.reason.is_empty() {
                    issue(
                        &mut c,
                        "NotDownloaded",
                        "Artifact is not downloaded and verified locally",
                    )
                }
                out.push(c);
            }
        }
        let ready = out.iter().filter(|c| c.status == "Ready").count();
        if ready > 1 {
            for c in &mut out {
                if c.status == "Ready" {
                    c.reason = "Explicit association and entry-point selection required".into()
                }
            }
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
fn open_nofollow(root: &Path, relative: &str, directory: bool) -> Result<File, LibraryError> {
    use std::os::unix::ffi::OsStrExt;
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };
    if !relative.is_empty() && !path_valid(relative) {
        return Err(invalid("unsafe relative launch path"));
    }
    let mut fd = File::open("/")?;
    if !root.is_absolute() {
        return Err(invalid("local launch root is not absolute"));
    }
    let mut components = Vec::new();
    for component in root.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(part) => components.push(part.to_os_string()),
            _ => return Err(invalid("unsafe local launch root")),
        }
    }
    if !relative.is_empty() {
        components.extend(relative.split('/').map(std::ffi::OsString::from));
    }
    for (i, part) in components.iter().enumerate() {
        let name = CString::new(part.as_bytes()).map_err(|_| invalid("NUL in local path"))?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if i + 1 < components.len() || directory {
                libc::O_DIRECTORY
            } else {
                0
            };
        let raw = unsafe { libc::openat(fd.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        fd = unsafe { File::from_raw_fd(raw) };
    }
    Ok(fd)
}

impl LaunchCandidate {
    #[cfg(target_os = "linux")]
    fn local_readiness(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let root = self
            .local_root
            .as_ref()
            .ok_or("verified local root missing")?;
        let target = self.target.as_deref().ok_or("declared target missing")?;
        let work = self
            .working_directory
            .as_deref()
            .ok_or("working directory missing")?;
        let file = open_nofollow(root, target, false)
            .map_err(|e| format!("unsafe or missing target: {e}"))?;
        open_nofollow(root, work, true)
            .map_err(|e| format!("unsafe or missing working directory: {e}"))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || Some(meta.len()) != self.expected_size {
            return Err("local target is missing or has changed size".into());
        }
        if meta.permissions().mode() & 0o111 == 0 {
            return Err("declared target is not executable".into());
        }
        Ok(())
    }
    /// Explicit Play only. Returns a child handle; caller owns waiting/reaping.
    pub fn spawn(&self) -> Result<Child, LibraryError> {
        if self.status != "Ready" {
            return Err(invalid("launch candidate is not ready"));
        }
        #[cfg(not(target_os = "linux"))]
        {
            return Err(invalid(
                "NativePortableV1 execution is available only on Linux",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::{fs::PermissionsExt, process::CommandExt};
            let root = self
                .local_root
                .as_ref()
                .ok_or_else(|| invalid("verified local root missing"))?;
            let target = self
                .target
                .as_deref()
                .ok_or_else(|| invalid("target missing"))?;
            let work = self
                .working_directory
                .as_deref()
                .ok_or_else(|| invalid("working directory missing"))?;
            if !path_valid(target) || (!work.is_empty() && !path_valid(work)) {
                return Err(invalid("unsafe launch path"));
            }
            let mut executable = open_nofollow(root, target, false)?;
            let dir = open_nofollow(root, work, true)?;
            let meta = executable.metadata()?;
            if !meta.is_file() || Some(meta.len()) != self.expected_size {
                return Err(invalid("local executable is missing or changed"));
            }
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(invalid("declared target is not executable by current user"));
            }
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = executable.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            let actual = format!("{:x}", hash.finalize());
            if self.expected_hash.as_deref() != Some(actual.as_str()) {
                return Err(invalid("local executable integrity check failed"));
            }
            // /proc/self/fd binds exec to the opened, hashed inode. The descriptor closes on exec.
            let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
            command.args(&self.arguments);
            for (key, _) in std::env::vars_os() {
                if sensitive_env_name(&key) {
                    command.env_remove(key);
                }
            }
            let dir_fd = dir.as_raw_fd();
            unsafe {
                command.pre_exec(move || {
                    if libc::fchdir(dir_fd) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            command.spawn().map_err(LibraryError::Io)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normative_fields_fail_closed() {
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            assert_eq!(host(), Some("linux/x86_64"));
        }
        assert!(path_valid("System64/unreal-bin-amd64"));
        for path in [
            "../host",
            "/absolute",
            "a//b",
            "a/./b",
            "a\\b",
            "C:/host",
            "",
        ] {
            assert!(!path_valid(path));
        }
        assert_eq!(arguments(None), Some(vec![]));
        assert_eq!(
            arguments(Some(&serde_json::json!(["", "$(touch /tmp/ignored)"]))),
            Some(vec!["".into(), "$(touch /tmp/ignored)".into()])
        );
        assert!(arguments(Some(&serde_json::json!(["a\n"]))).is_none());
        assert!(arguments(Some(&serde_json::json!(["x".repeat(4097)]))).is_none());
        assert!(valid_platforms(&serde_json::json!(["linux/amd64", "linux/x86_64"])).is_some());
        assert!(valid_platforms(&serde_json::json!(["linux/x86_64", "linux/x86_64"])).is_none());
        #[cfg(target_os = "linux")]
        {
            assert!(sensitive_env_name(std::ffi::OsStr::new("APP_DEPOT_TOKEN")));
            assert!(sensitive_env_name(std::ffi::OsStr::new("SSH_AUTH_SOCK")));
            assert!(!sensitive_env_name(std::ffi::OsStr::new(
                "DBUS_SESSION_BUS_ADDRESS"
            )));
        }
    }
    #[test]
    fn cached_snapshot_assesses_exact_method_platform_and_association() {
        use rusqlite::params;
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::open(crate::library::LibraryPaths {
            metadata: temp.path().join("meta"),
            content: temp.path().join("content"),
        })
        .unwrap();
        let profile = "11111111-1111-4111-8111-111111111111";
        lib.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,'Test','https://example.test','anonymous','offline','[]')",[profile]).unwrap();
        lib.conn
            .execute(
                "INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,'app',?2,0)",
                params![
                    profile,
                    r#"{"id":"app","name":"Test","visibility":"Public","state":"Published"}"#
                ],
            )
            .unwrap();
        lib.conn.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,'release','app',?2,0)",params![profile,r#"{"id":"release","appId":"app","state":"Published"}"#]).unwrap();
        let artifact = format!("adfile:sha256:{}", "a".repeat(64));
        lib.conn
            .execute(
                "INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,0)",
                params![
                    profile,
                    artifact,
                    serde_json::json!({"id":artifact,"kind":"File","size":1}).to_string()
                ],
            )
            .unwrap();
        for id in ["one", "two"] {
            lib.conn.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,'release',?3,?4,0)",params![profile,id,artifact,serde_json::json!({"id":id,"releaseId":"release","artifactId":artifact}).to_string()]).unwrap();
        }
        let points = serde_json::json!([
            {"id":"legacy","target":{"type":"ArtifactFile"}},
            {"id":"game","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","arguments":["", "$(echo literal)"]}},
            {"id":"alias","target":{"type":"ArtifactFile"},"platforms":["linux/amd64"],"launch":{"method":"NativePortableV1"}},
            {"id":"future","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV2"}},
            {"id":"malformed","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","arguments":["bad\n"]}}
        ]);
        let snapshot = serde_json::json!({"releaseId":"release","app":{"revisionId":"app-rev","data":{}},"release":{"revisionId":"release-rev","data":{"appDepot":{"platforms":["linux/x86_64"]}}},"associations":[
            {"associationId":"one","declaration":{"revisionId":"one-rev","data":{"appDepot":{"entryPoints":points},"unknown":18446744073709551615u64}}},
            {"associationId":"two","declaration":{"revisionId":"two-rev","data":{"appDepot":{"entryPoints":[{"id":"other","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1"}}]}}}}
        ]});
        lib.conn.execute("INSERT INTO snapshots(profile_id,release_id,snapshot_json,app_revision,release_revision,last_seen) VALUES (?1,'release',?2,'app-rev','release-rev',0)",params![profile,snapshot.to_string()]).unwrap();
        let result = lib
            .launch_candidates(&ServerProfileId(profile.into()), "release")
            .unwrap();
        assert_eq!(result.len(), 6);
        assert_eq!(result[0].status, "NoLaunch");
        assert_eq!(result[1].status, "NotDownloaded");
        assert_eq!(result[1].arguments, ["", "$(echo literal)"]);
        assert_eq!(result[2].status, "UnsupportedPlatform");
        assert_eq!(result[3].status, "UnsupportedMethod");
        assert_eq!(result[4].status, "Malformed");
        assert_eq!(result[5].association_id, "two");
        assert_eq!(result[5].status, "NotDownloaded");
        assert!(
            lib.cached_snapshot(&ServerProfileId(profile.into()), "release")
                .unwrap()
                .unwrap()
                .value
                .associations[0]
                .declaration
                .data
                .to_string()
                .contains("18446744073709551615")
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn direct_process_uses_verified_open_inode_and_rejects_symlink() {
        use std::{io::Read, os::unix::fs::symlink};
        let temp = tempfile::tempdir().unwrap();
        let source = std::env::current_exe().unwrap();
        let target = temp.path().join("native-program");
        std::fs::copy(source, &target).unwrap();
        let mut bytes = Vec::new();
        File::open(&target)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let candidate = LaunchCandidate {
            profile_id: ServerProfileId("test".into()),
            release_id: "r".into(),
            association_id: "a".into(),
            artifact_id: "artifact".into(),
            entry_id: "entry".into(),
            declaration_revision: "test-revision".into(),
            label: None,
            target: Some("native-program".into()),
            working_directory: Some(String::new()),
            arguments: vec!["--help".into()],
            status: "Ready".into(),
            reason: String::new(),
            local_root: Some(temp.path().to_owned()),
            expected_hash: Some(format!("{:x}", Sha256::digest(&bytes))),
            expected_size: Some(bytes.len() as u64),
        };
        assert!(candidate.spawn().unwrap().wait().unwrap().success());
        std::fs::remove_file(&target).unwrap();
        symlink("/bin/true", &target).unwrap();
        assert!(candidate.spawn().is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn tree_process_preserves_literal_argv_workdir_and_strips_secrets() {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
        };
        let temp = tempfile::tempdir().unwrap();
        let system = temp.path().join("System64");
        fs::create_dir(&system).unwrap();
        let source = temp.path().join("fixture.rs");
        fs::write(
            &source,
            r#"fn main() {
                let args: Vec<_> = std::env::args().skip(1).collect();
                let cwd = std::env::current_dir().unwrap();
                let secret = std::env::var_os("APP_DEPOT_TEST_SECRET").is_some();
                std::fs::write("report.txt", format!("{args:?}\n{}\n{secret}\n", cwd.display())).unwrap();
                std::process::exit(7);
            }"#,
        )
        .unwrap();
        let executable = system.join("game");
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
        let bytes = fs::read(&executable).unwrap();
        let candidate = LaunchCandidate {
            profile_id: ServerProfileId("test".into()),
            release_id: "r".into(),
            association_id: "a".into(),
            artifact_id: "tree".into(),
            entry_id: "game".into(),
            declaration_revision: "test-revision".into(),
            label: None,
            target: Some("System64/game".into()),
            working_directory: Some("System64".into()),
            arguments: vec!["literal;$(echo hi)".into(), String::new()],
            status: "Ready".into(),
            reason: String::new(),
            local_root: Some(temp.path().to_owned()),
            expected_hash: Some(format!("{:x}", Sha256::digest(&bytes))),
            expected_size: Some(bytes.len() as u64),
        };
        assert!(candidate.local_readiness().is_ok());
        let status = candidate.spawn().unwrap().wait().unwrap();
        assert_eq!(status.code(), Some(7));
        let report = fs::read_to_string(system.join("report.txt")).unwrap();
        assert!(report.contains("[\"literal;$(echo hi)\", \"\"]"));
        assert!(report.contains(&system.display().to_string()));
        assert!(report.ends_with("false\n"));
        fs::write(&executable, b"corrupt").unwrap();
        assert!(candidate.spawn().is_err());
        fs::write(&executable, &bytes).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(candidate.spawn().is_err());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&system, temp.path().join("linked-directory")).unwrap();
        let mut unsafe_directory = candidate.clone();
        unsafe_directory.working_directory = Some("linked-directory".into());
        assert!(unsafe_directory.spawn().is_err());
        let mut unsafe_target = candidate;
        unsafe_target.target = Some("../System64/game".into());
        assert!(unsafe_target.spawn().is_err());
    }
}
