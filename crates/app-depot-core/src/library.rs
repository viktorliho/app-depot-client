//! Local profile and catalog metadata. No Server paths or Bearer secrets are stored here.

use crate::{ServerProfileId, api::ApiClient, models::*};
use directories::BaseDirs;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    fmt, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 5;
const MARKER: &str = ".app-depot-library-id";

#[derive(Debug)]
pub enum LibraryError {
    Invalid(String),
    Io(std::io::Error),
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Api(crate::api::ApiError),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "local library: {message}"),
            Self::Io(error) => write!(f, "local library I/O: {error}"),
            Self::Sql(error) => write!(f, "local library SQLite: {error}"),
            Self::Json(error) => write!(f, "local library JSON: {error}"),
            Self::Api(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for LibraryError {}
impl From<std::io::Error> for LibraryError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<rusqlite::Error> for LibraryError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}
impl From<serde_json::Error> for LibraryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
impl From<crate::api::ApiError> for LibraryError {
    fn from(value: crate::api::ApiError) -> Self {
        Self::Api(value)
    }
}

#[derive(Clone, Debug)]
pub struct LibraryPaths {
    pub metadata: PathBuf,
    pub content: PathBuf,
}

impl LibraryPaths {
    pub fn default_for_user() -> Result<Self, LibraryError> {
        let dirs = BaseDirs::new()
            .ok_or_else(|| LibraryError::Invalid("user data directory unavailable".into()))?;
        #[cfg(target_os = "linux")]
        let base = {
            let legacy = dirs.data_dir().join("app-depot-client");
            let current = dirs.home_dir().join(".appdepot");
            let legacy_id = read_marker(&legacy)?;
            let current_id = read_marker(&current)?;
            match (current_id, legacy_id) {
                (Some(current_id), Some(legacy_id)) if current_id != legacy_id => {
                    return Err(invalid(
                        "two distinct default-location libraries exist; select one explicitly",
                    ));
                }
                (Some(_), _) => current,
                (None, Some(_)) => legacy,
                (None, None) => current,
            }
        };
        #[cfg(not(target_os = "linux"))]
        let base = dirs.data_dir().join("app-depot-client");
        Ok(Self {
            metadata: base.clone(),
            content: base,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: ServerProfileId,
    pub name: String,
    pub origin: String,
    pub auth_mode: String,
    pub credential_ref: Option<String>,
    pub last_connection: Option<i64>,
    pub last_sync: Option<i64>,
    pub capabilities: Option<Capabilities>,
    pub connection_status: String,
    pub compatibility_issues: Vec<String>,
    pub archived: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cached<T> {
    pub profile_id: ServerProfileId,
    pub value: T,
    pub last_seen: i64,
    pub last_sync: Option<i64>,
    pub stale: bool,
    pub source: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalIssue {
    pub profile_id: ServerProfileId,
    pub association_id: Option<String>,
    pub artifact_id: String,
    pub kind: String,
    pub reason: String,
}

pub struct Library {
    pub(crate) conn: Connection,
    pub paths: LibraryPaths,
    pub id: String,
    _access_lock: fs::File,
}

pub(crate) fn now() -> Result<i64, LibraryError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LibraryError::Invalid("system time before Unix epoch".into()))?
        .as_millis() as i64)
}

fn invalid(message: &str) -> LibraryError {
    LibraryError::Invalid(message.into())
}

pub(crate) fn ensure_directory(path: &Path) -> Result<(), LibraryError> {
    let path_text = path.as_os_str().to_string_lossy();
    if !path.is_absolute()
        || path_text
            .split(['/', '\\'])
            .any(|segment| matches!(segment, "." | ".."))
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || path == Path::new("/")
    {
        return Err(invalid(
            "library roots must be absolute, non-root paths without . or ..",
        ));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid("symlink in library path"));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(invalid("non-directory in library path"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_private_root(path: &Path) -> Result<(), LibraryError> {
    ensure_directory(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "library root must not be accessible by group or others",
            ));
        }
    }
    Ok(())
}

pub(crate) fn access_lock(root: &Path, exclusive: bool) -> Result<fs::File, LibraryError> {
    use fs2::FileExt;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(root.join(".app-depot-access.lock"))?;
    if !file.metadata()?.is_file() {
        return Err(invalid("unsafe library access lock"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(invalid("library access lock must be private"));
        }
    }
    let locked = if exclusive {
        FileExt::try_lock_exclusive(&file)
    } else {
        FileExt::try_lock_shared(&file)
    };
    locked.map_err(|_| invalid("library is in use by another operation"))?;
    Ok(file)
}

fn schema_open_lock(root: &Path) -> Result<fs::File, LibraryError> {
    use fs2::FileExt;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(root.join(".app-depot-schema.lock"))?;
    if !file.metadata()?.is_file() {
        return Err(invalid("unsafe schema lock"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(invalid("schema lock must be private"));
        }
    }
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

pub(crate) fn read_marker(path: &Path) -> Result<Option<String>, LibraryError> {
    let marker = path.join(MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err(invalid("unsafe library identity marker"))
        }
        Ok(metadata) if metadata.len() > 64 => Err(invalid("invalid library identity marker")),
        Ok(_) => {
            let mut value = String::new();
            fs::File::open(marker)?.read_to_string(&mut value)?;
            if Uuid::parse_str(value.trim()).is_err() {
                return Err(invalid("invalid library identity marker"));
            }
            Ok(Some(value.trim().into()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn write_marker(path: &Path, id: &str) -> Result<(), LibraryError> {
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join(MARKER))?;
    file.write_all(id.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

impl Library {
    pub fn open(paths: LibraryPaths) -> Result<Self, LibraryError> {
        ensure_private_root(&paths.metadata)?;
        ensure_private_root(&paths.content)?;
        let schema_lock = schema_open_lock(&paths.metadata)?;
        let access = access_lock(&paths.metadata, false)?;
        let metadata_id = read_marker(&paths.metadata)?;
        let content_id = read_marker(&paths.content)?;
        if metadata_id.is_none() && paths.metadata.join("library.sqlite").exists() {
            return Err(invalid("existing database has no library identity marker"));
        }
        if metadata_id.is_some() && content_id.is_some() && metadata_id != content_id {
            return Err(invalid(
                "metadata and content roots belong to different libraries",
            ));
        }
        let id = metadata_id
            .or(content_id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        if read_marker(&paths.metadata)?.is_none() {
            write_marker(&paths.metadata, &id)?;
        }
        if paths.content != paths.metadata && read_marker(&paths.content)?.is_none() {
            write_marker(&paths.content, &id)?;
        }
        for directory in [
            "objects",
            "artifacts",
            "staging",
            "artwork",
            "apps",
            "userdata",
        ] {
            ensure_directory(&paths.content.join(directory))?;
        }
        let db = paths.metadata.join("library.sqlite");
        let existing_db = match fs::symlink_metadata(&db) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err(invalid("unsafe SQLite database path"));
            }
            Ok(meta) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err(invalid(
                            "SQLite database must not be accessible by group or others",
                        ));
                    }
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let conn = Connection::open(&db)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !existing_db {
                fs::set_permissions(&db, fs::Permissions::from_mode(0o600))?;
            }
        }
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(invalid("library schema is newer than this client"));
        }
        if version == 0 {
            let table_count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0))?;
            if table_count != 0 {
                return Err(invalid("unrecognized existing SQLite schema"));
            }
            conn.execute_batch(SCHEMA)?;
            conn.execute_batch(MIGRATE_V2)?;
            conn.execute_batch(MIGRATE_V3)?;
            conn.execute_batch(MIGRATE_V4)?;
            conn.execute_batch(MIGRATE_V5)?;
        } else if version == 1 {
            conn.execute_batch(MIGRATE_V2)?;
            conn.execute_batch(MIGRATE_V3)?;
            conn.execute_batch(MIGRATE_V4)?;
            conn.execute_batch(MIGRATE_V5)?;
        } else if version == 2 {
            conn.execute_batch(MIGRATE_V3)?;
            conn.execute_batch(MIGRATE_V4)?;
            conn.execute_batch(MIGRATE_V5)?;
        } else if version == 3 {
            conn.execute_batch(MIGRATE_V4)?;
            conn.execute_batch(MIGRATE_V5)?;
        } else if version == 4 {
            conn.execute_batch(MIGRATE_V5)?;
        } else if version != SCHEMA_VERSION {
            return Err(invalid("library schema migration is required"));
        }
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM library_meta WHERE key='library_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(stored) if stored != id => {
                return Err(invalid("SQLite library identity does not match root"));
            }
            None => {
                conn.execute(
                    "INSERT INTO library_meta(key,value) VALUES ('library_id',?1)",
                    [&id],
                )?;
            }
            _ => {}
        }
        if !existing_db {
            conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        }
        conn.execute_batch("PRAGMA synchronous=FULL;")?;
        drop(schema_lock);
        let mut library = Self {
            conn,
            paths,
            id,
            _access_lock: access,
        };
        library.recover_removals()?;
        Ok(library)
    }

    pub fn add_profile(
        &mut self,
        name: &str,
        url: &str,
        allow_http: bool,
    ) -> Result<Profile, LibraryError> {
        if name.trim().is_empty() {
            return Err(invalid("profile name is empty"));
        }
        let origin = ApiClient::new(url, None, allow_http)?.origin().to_owned();
        let id = Uuid::new_v4().to_string();
        self.conn.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,?2,?3,'anonymous','never','[]')", params![id, name, origin])?;
        self.profile(&ServerProfileId(id))
    }

    pub fn profile(&self, id: &ServerProfileId) -> Result<Profile, LibraryError> {
        let raw = self.conn.query_row("SELECT name,origin,auth_mode,credential_ref,last_connection,last_sync,capabilities_json,connection_status,compatibility_json,archived FROM profiles WHERE id=?1", [&id.0], |row| {
            Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,Option<String>>(3)?, row.get::<_,Option<i64>>(4)?, row.get::<_,Option<i64>>(5)?, row.get::<_,Option<String>>(6)?, row.get::<_,String>(7)?, row.get::<_,String>(8)?, row.get::<_,bool>(9)?))
        }).optional()?.ok_or_else(|| invalid("profile not found"))?;
        Ok(Profile {
            id: id.clone(),
            name: raw.0,
            origin: raw.1,
            auth_mode: raw.2,
            credential_ref: raw.3,
            last_connection: raw.4,
            last_sync: raw.5,
            capabilities: raw
                .6
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            connection_status: raw.7,
            compatibility_issues: serde_json::from_str(&raw.8)?,
            archived: raw.9,
        })
    }

    pub fn release_for_association(
        &self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<Option<String>, LibraryError> {
        Ok(self
            .conn
            .query_row(
                "SELECT release_id FROM associations WHERE profile_id=?1 AND id=?2",
                params![profile.0, association],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn display_context_for_association(
        &self,
        profile: &ServerProfileId,
        association: &str,
    ) -> Result<Option<(String, String, String)>, LibraryError> {
        let raw:Option<(String,String,String)>=self.conn.query_row("SELECT apps.metadata_json,releases.metadata_json,profiles.name FROM associations JOIN releases ON releases.profile_id=associations.profile_id AND releases.id=associations.release_id JOIN apps ON apps.profile_id=releases.profile_id AND apps.id=releases.app_id JOIN profiles ON profiles.id=associations.profile_id WHERE associations.profile_id=?1 AND associations.id=?2",params![profile.0,association],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        raw.map(|(app, release, server)| {
            let app: serde_json::Value = serde_json::from_str(&app)?;
            let release: serde_json::Value = serde_json::from_str(&release)?;
            Ok((
                app.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Saved application")
                    .to_owned(),
                release
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Release")
                    .to_owned(),
                server,
            ))
        })
        .transpose()
    }

    pub fn profiles(&self, include_archived: bool) -> Result<Vec<Profile>, LibraryError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM profiles WHERE archived=0 OR ?1 ORDER BY name,id")?;
        let ids = stmt
            .query_map([include_archived], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.profile(&ServerProfileId(id)))
            .collect()
    }

    pub fn rename_profile(&mut self, id: &ServerProfileId, name: &str) -> Result<(), LibraryError> {
        if name.trim().is_empty() {
            return Err(invalid("profile name is empty"));
        }
        if self.conn.execute(
            "UPDATE profiles SET name=?2 WHERE id=?1 AND archived=0",
            params![id.0, name],
        )? == 0
        {
            return Err(invalid("active profile not found"));
        }
        Ok(())
    }

    /// A changed origin always gets a new identity. The old profile and cache stay archived.
    pub fn replace_origin(
        &mut self,
        id: &ServerProfileId,
        url: &str,
        allow_http: bool,
    ) -> Result<Profile, LibraryError> {
        let old = self.profile(id)?;
        if old.archived {
            return Err(invalid("cannot edit an archived profile"));
        }
        if old.credential_ref.is_some() {
            return Err(invalid(
                "remove the old profile token before changing its URL",
            ));
        }
        let origin = ApiClient::new(url, None, allow_http)?.origin().to_owned();
        if origin == old.origin {
            return Ok(old);
        }
        let next = Uuid::new_v4().to_string();
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE profiles SET archived=1 WHERE id=?1", [&id.0])?;
        tx.execute("INSERT INTO profiles(id,name,origin,auth_mode,connection_status,compatibility_json) VALUES (?1,?2,?3,'anonymous','never','[]')", params![next,old.name,origin])?;
        tx.commit()?;
        self.profile(&ServerProfileId(next))
    }

    pub fn archive_profile(&mut self, id: &ServerProfileId) -> Result<(), LibraryError> {
        if self.conn.execute(
            "UPDATE profiles SET archived=1 WHERE id=?1 AND archived=0 AND credential_ref IS NULL",
            [&id.0],
        )? == 0
        {
            return Err(invalid(
                "active profile not found or token must be removed first",
            ));
        }
        Ok(())
    }

    pub fn set_credential_ref(
        &mut self,
        id: &ServerProfileId,
        reference: Option<&str>,
    ) -> Result<(), LibraryError> {
        if reference.is_some_and(|reference| Uuid::parse_str(reference).is_err()) {
            return Err(invalid("credential reference must be a generated UUID"));
        }
        let mode = if reference.is_some() {
            "secure"
        } else {
            "anonymous"
        };
        if self.conn.execute(
            "UPDATE profiles SET auth_mode=?2,credential_ref=?3 WHERE id=?1 AND archived=0",
            params![id.0, mode, reference],
        )? == 0
        {
            return Err(invalid("active profile not found"));
        }
        Ok(())
    }

    pub fn connection_result(
        &mut self,
        id: &ServerProfileId,
        caps: Option<&Capabilities>,
        status: &str,
    ) -> Result<(), LibraryError> {
        let current = self.profile(id)?;
        let issues = caps
            .map(Capabilities::incompatibilities)
            .unwrap_or(current.compatibility_issues);
        let timestamp = if caps.is_some() {
            Some(now()?)
        } else {
            current.last_connection
        };
        self.conn.execute("UPDATE profiles SET capabilities_json=COALESCE(?2,capabilities_json),last_connection=?3,connection_status=?4,compatibility_json=?5 WHERE id=?1", params![id.0,caps.map(serde_json::to_string).transpose()?,timestamp,status,serde_json::to_string(&issues)?])?;
        Ok(())
    }

    pub fn cached_apps(&self, id: &ServerProfileId) -> Result<Vec<Cached<App>>, LibraryError> {
        self.cached_rows(id, "apps", None)
    }
    pub fn cached_releases(
        &self,
        id: &ServerProfileId,
        app_id: &str,
    ) -> Result<Vec<Cached<Release>>, LibraryError> {
        self.cached_rows(id, "releases", Some(app_id))
    }
    pub fn cached_associations(
        &self,
        id: &ServerProfileId,
        release_id: &str,
    ) -> Result<Vec<Cached<Association>>, LibraryError> {
        self.cached_rows(id, "associations", Some(release_id))
    }
    pub fn cached_artifact(
        &self,
        id: &ServerProfileId,
        artifact_id: &str,
    ) -> Result<Option<Cached<Artifact>>, LibraryError> {
        let profile = self.profile(id)?;
        let raw: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT metadata_json,last_seen FROM artifacts WHERE profile_id=?1 AND id=?2",
                params![id.0, artifact_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        raw.map(|(json, last_seen)| {
            Ok(Cached {
                profile_id: id.clone(),
                value: serde_json::from_str(&json)?,
                last_seen,
                last_sync: profile.last_sync,
                stale: profile.last_sync != Some(last_seen),
                source: "cache",
            })
        })
        .transpose()
    }
    pub fn cached_snapshot(
        &self,
        id: &ServerProfileId,
        release_id: &str,
    ) -> Result<Option<Cached<DeclarationSnapshot>>, LibraryError> {
        let profile = self.profile(id)?;
        let raw: Option<(String,i64)> = self.conn.query_row("SELECT snapshot_json,last_seen FROM snapshots WHERE profile_id=?1 AND release_id=?2", params![id.0,release_id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        raw.map(|(json, last_seen)| {
            Ok(Cached {
                profile_id: id.clone(),
                value: serde_json::from_str(&json)?,
                last_seen,
                last_sync: profile.last_sync,
                stale: profile.last_sync != Some(last_seen),
                source: "cache",
            })
        })
        .transpose()
    }

    fn cached_rows<T: DeserializeOwned>(
        &self,
        id: &ServerProfileId,
        table: &str,
        parent: Option<&str>,
    ) -> Result<Vec<Cached<T>>, LibraryError> {
        let profile = self.profile(id)?;
        let (sql, parent) = match (table, parent) {
            ("apps", _) => (
                "SELECT metadata_json,last_seen FROM apps WHERE profile_id=?1 ORDER BY id",
                "",
            ),
            ("releases", Some(parent)) => (
                "SELECT metadata_json,last_seen FROM releases WHERE profile_id=?1 AND app_id=?2 ORDER BY id",
                parent,
            ),
            ("associations", Some(parent)) => (
                "SELECT metadata_json,last_seen FROM associations WHERE profile_id=?1 AND release_id=?2 ORDER BY id",
                parent,
            ),
            _ => return Err(invalid("invalid cache query")),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let parse = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, i64)> {
            Ok((row.get(0)?, row.get(1)?))
        };
        let rows = if table == "apps" {
            stmt.query_map([&id.0], parse)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![id.0, parent], parse)?
                .collect::<Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(|(json, last_seen)| {
                Ok(Cached {
                    profile_id: id.clone(),
                    value: serde_json::from_str(&json)?,
                    last_seen,
                    last_sync: profile.last_sync,
                    stale: profile.last_sync != Some(last_seen),
                    source: "cache",
                })
            })
            .collect()
    }

    pub fn sync(
        &mut self,
        id: &ServerProfileId,
        client: &ApiClient,
    ) -> Result<SyncSummary, LibraryError> {
        let profile = self.profile(id)?;
        if profile.archived {
            return Err(invalid("archived profiles cannot synchronize"));
        }
        if profile.origin != client.origin() {
            return Err(invalid("client origin does not match profile"));
        }
        let fetched = fetch_catalog(client);
        let fetched = match fetched {
            Ok(value) => value,
            Err(error) => {
                let status = match &error {
                    LibraryError::Api(crate::api::ApiError::Transport(_)) => "unreachable",
                    LibraryError::Api(crate::api::ApiError::Authentication(_)) => {
                        "authentication-required"
                    }
                    LibraryError::Api(crate::api::ApiError::Authorization(_)) => "forbidden",
                    LibraryError::Api(crate::api::ApiError::Incompatible(_)) => "incompatible",
                    LibraryError::Api(crate::api::ApiError::Protocol(_))
                    | LibraryError::Invalid(_) => "protocol-error",
                    _ => "http-error",
                };
                self.connection_result(id, None, status)?;
                return Err(error);
            }
        };
        let time = now()?.max(profile.last_sync.unwrap_or(0).saturating_add(1));
        let tx = self.conn.transaction()?;
        for app in &fetched.apps {
            tx.execute("INSERT INTO apps(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,?4) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json,last_seen=excluded.last_seen", params![id.0,app.id,serde_json::to_string(app)?,time])?;
        }
        for release in &fetched.releases {
            tx.execute("INSERT INTO releases(profile_id,id,app_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(profile_id,id) DO UPDATE SET app_id=excluded.app_id,metadata_json=excluded.metadata_json,last_seen=excluded.last_seen", params![id.0,release.id,release.app_id,serde_json::to_string(release)?,time])?;
        }
        for item in &fetched.artifacts {
            tx.execute("INSERT INTO artifacts(profile_id,id,metadata_json,last_seen) VALUES (?1,?2,?3,?4) ON CONFLICT(profile_id,id) DO UPDATE SET metadata_json=excluded.metadata_json,last_seen=excluded.last_seen", params![id.0,item.id,serde_json::to_string(item)?,time])?;
        }
        for item in &fetched.associations {
            tx.execute("INSERT INTO associations(profile_id,id,release_id,artifact_id,metadata_json,last_seen) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(profile_id,id) DO UPDATE SET release_id=excluded.release_id,artifact_id=excluded.artifact_id,metadata_json=excluded.metadata_json,last_seen=excluded.last_seen", params![id.0,item.id,item.release_id,item.artifact_id,serde_json::to_string(item)?,time])?;
        }
        for (snapshot, etag) in &fetched.snapshots {
            tx.execute("INSERT INTO snapshots(profile_id,release_id,snapshot_json,app_revision,release_revision,etag,last_seen) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(profile_id,release_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,app_revision=excluded.app_revision,release_revision=excluded.release_revision,etag=excluded.etag,last_seen=excluded.last_seen", params![id.0,snapshot.release_id,serde_json::to_string(snapshot)?,snapshot.app.revision_id,snapshot.release.revision_id,etag,time])?;
        }
        tx.execute("UPDATE profiles SET last_sync=?2,last_connection=?2,capabilities_json=?3,connection_status='connected',compatibility_json=?4 WHERE id=?1", params![id.0,time,serde_json::to_string(&fetched.capabilities)?,serde_json::to_string(&fetched.capabilities.incompatibilities())?])?;
        tx.commit()?;
        Ok(SyncSummary {
            apps: fetched.apps.len(),
            releases: fetched.releases.len(),
            associations: fetched.associations.len(),
            artifacts: fetched.artifacts.len(),
            at: time,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncSummary {
    pub apps: usize,
    pub releases: usize,
    pub associations: usize,
    pub artifacts: usize,
    pub at: i64,
}

struct FetchedCatalog {
    capabilities: Capabilities,
    apps: Vec<App>,
    releases: Vec<Release>,
    associations: Vec<Association>,
    artifacts: Vec<Artifact>,
    snapshots: Vec<(DeclarationSnapshot, Option<String>)>,
}

fn fetch_catalog(client: &ApiClient) -> Result<FetchedCatalog, LibraryError> {
    let capabilities = client.ensure_read_compatible()?;
    let page_size = capabilities
        .page_size()
        .map_err(crate::api::ApiError::Incompatible)?;
    let apps = client.list_apps(page_size)?;
    let mut releases = Vec::new();
    let mut associations = Vec::new();
    let mut artifacts = Vec::new();
    let mut snapshots = Vec::new();
    let mut seen_artifacts = HashSet::new();
    for app in &apps {
        for release in client.list_releases(&app.id, page_size)? {
            let associated = client.associated_artifacts(&release.id, page_size)?;
            let (snapshot, etag) = client.declarations_with_etag(&release.id)?;
            let association_ids: HashSet<_> = associated
                .iter()
                .map(|item| item.association.id.as_str())
                .collect();
            let snapshot_ids: HashSet<_> = snapshot
                .associations
                .iter()
                .map(|item| item.association_id.as_str())
                .collect();
            if association_ids != snapshot_ids {
                return Err(invalid(
                    "Release associations changed during synchronization",
                ));
            }
            for item in associated {
                if seen_artifacts.insert(item.artifact.id.clone()) {
                    artifacts.push(item.artifact);
                }
                associations.push(item.association);
            }
            snapshots.push((snapshot, etag));
            releases.push(release);
        }
    }
    Ok(FetchedCatalog {
        capabilities,
        apps,
        releases,
        associations,
        artifacts,
        snapshots,
    })
}

const SCHEMA: &str = r#"
BEGIN IMMEDIATE;
CREATE TABLE library_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL, origin TEXT NOT NULL, auth_mode TEXT NOT NULL CHECK(auth_mode IN ('anonymous','secure')), credential_ref TEXT, last_connection INTEGER, last_sync INTEGER, capabilities_json TEXT, connection_status TEXT NOT NULL, compatibility_json TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)));
CREATE TABLE apps (profile_id TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL, metadata_json TEXT NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY(profile_id,id));
CREATE TABLE releases (profile_id TEXT NOT NULL, id TEXT NOT NULL, app_id TEXT NOT NULL, metadata_json TEXT NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY(profile_id,id), FOREIGN KEY(profile_id,app_id) REFERENCES apps(profile_id,id));
CREATE TABLE artifacts (profile_id TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL, metadata_json TEXT NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY(profile_id,id));
CREATE TABLE associations (profile_id TEXT NOT NULL, id TEXT NOT NULL, release_id TEXT NOT NULL, artifact_id TEXT NOT NULL, metadata_json TEXT NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY(profile_id,id), FOREIGN KEY(profile_id,release_id) REFERENCES releases(profile_id,id), FOREIGN KEY(profile_id,artifact_id) REFERENCES artifacts(profile_id,id));
CREATE TABLE snapshots (profile_id TEXT NOT NULL, release_id TEXT NOT NULL, snapshot_json TEXT NOT NULL, app_revision TEXT NOT NULL, release_revision TEXT NOT NULL, etag TEXT, last_seen INTEGER NOT NULL, PRIMARY KEY(profile_id,release_id), FOREIGN KEY(profile_id,release_id) REFERENCES releases(profile_id,id));
CREATE TABLE verified_content (profile_id TEXT NOT NULL, artifact_id TEXT NOT NULL, local_ref TEXT NOT NULL, verified_at INTEGER NOT NULL, PRIMARY KEY(profile_id,artifact_id,local_ref), FOREIGN KEY(profile_id,artifact_id) REFERENCES artifacts(profile_id,id));
CREATE TABLE download_jobs (id TEXT PRIMARY KEY, profile_id TEXT NOT NULL REFERENCES profiles(id), artifact_id TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL, FOREIGN KEY(profile_id,artifact_id) REFERENCES artifacts(profile_id,id));
PRAGMA user_version=1;
COMMIT;
"#;

const MIGRATE_V2: &str = r#"
BEGIN IMMEDIATE;
ALTER TABLE download_jobs ADD COLUMN association_id TEXT;
ALTER TABLE download_jobs ADD COLUMN expected_size INTEGER;
ALTER TABLE download_jobs ADD COLUMN etag TEXT;
ALTER TABLE download_jobs ADD COLUMN progress INTEGER NOT NULL DEFAULT 0;
ALTER TABLE download_jobs ADD COLUMN checkpoint_hash TEXT;
ALTER TABLE download_jobs ADD COLUMN last_error TEXT;
ALTER TABLE download_jobs ADD COLUMN updated_at INTEGER;
ALTER TABLE download_jobs ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE verified_content ADD COLUMN association_id TEXT;
CREATE UNIQUE INDEX download_jobs_active ON download_jobs(profile_id,artifact_id,association_id) WHERE state IN ('Queued','Downloading','Verifying','Committing','Paused');
PRAGMA user_version=2;
COMMIT;
"#;

const MIGRATE_V3: &str = r#"
BEGIN IMMEDIATE;
ALTER TABLE download_jobs ADD COLUMN inventory_json TEXT;
ALTER TABLE download_jobs ADD COLUMN inventory_etag TEXT;
DROP INDEX download_jobs_active;
CREATE UNIQUE INDEX download_jobs_active ON download_jobs(profile_id,artifact_id,association_id) WHERE state IN ('Queued','FetchingInventory','Downloading','Verifying','Committing','Paused');
CREATE TABLE tree_files (
  job_id TEXT NOT NULL REFERENCES download_jobs(id),
  path TEXT NOT NULL,
  file_id TEXT NOT NULL,
  size INTEGER NOT NULL CHECK(size >= 0),
  sha256 TEXT NOT NULL,
  executable INTEGER NOT NULL CHECK(executable IN (0,1)),
  progress INTEGER NOT NULL DEFAULT 0 CHECK(progress >= 0),
  etag TEXT,
  checkpoint_hash TEXT,
  verified INTEGER NOT NULL DEFAULT 0 CHECK(verified IN (0,1)),
  PRIMARY KEY(job_id,path), UNIQUE(job_id,file_id)
);
PRAGMA user_version=3;
COMMIT;
"#;

const MIGRATE_V4: &str = r#"
BEGIN IMMEDIATE;
CREATE TABLE installations (
  id TEXT PRIMARY KEY,
  profile_id TEXT NOT NULL,
  app_id TEXT NOT NULL,
  release_id TEXT NOT NULL,
  association_id TEXT NOT NULL,
  artifact_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('Preparing','Ready','Removing','Removed','NeedsRepair')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  last_error TEXT,
  process_id INTEGER,
  process_start TEXT,
  remove_complete INTEGER NOT NULL DEFAULT 0 CHECK(remove_complete IN (0,1)),
  UNIQUE(profile_id,association_id,artifact_id),
  FOREIGN KEY(profile_id,association_id) REFERENCES associations(profile_id,id)
);
PRAGMA user_version=4;
COMMIT;
"#;

const MIGRATE_V5: &str = r#"
BEGIN IMMEDIATE;
ALTER TABLE download_jobs ADD COLUMN queue_rank INTEGER NOT NULL DEFAULT 0;
ALTER TABLE download_jobs ADD COLUMN auto_resume INTEGER NOT NULL DEFAULT 0 CHECK(auto_resume IN (0,1));
UPDATE download_jobs SET queue_rank=rowid;
PRAGMA user_version=5;
COMMIT;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn paths(dir: &tempfile::TempDir) -> LibraryPaths {
        LibraryPaths {
            metadata: dir.path().join("metadata"),
            content: dir.path().join("content"),
        }
    }
    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CAPS: &str = r#"{"apiVersion":"1.0.0","ingestionModes":[],"zipMethods":[],"treeIdentity":"adtree:v1:sha256","fileIdentity":"adfile:sha256","uploadRanges":true,"downloadRanges":true,"limits":{"maxPageSize":1000,"maxConcurrentRanges":8,"maxUploadBytes":18446744073709551615},"features":[]}"#;
    fn replies() -> Vec<(u16, String)> {
        vec![
            (200,CAPS.into()),
            (200,r#"{"items":[{"id":"app","name":"Example","slug":"example","ownerId":"owner","visibility":"Public","state":"Published","future":18446744073709551616}],"nextCursor":null}"#.into()),
            (200,r#"{"items":[{"id":"release","appId":"app","state":"Published","version":"1"}],"nextCursor":null}"#.into()),
            (200,format!(r#"{{"items":[{{"id":"assoc","releaseId":"release","artifactId":"adfile:sha256:{DIGEST}"}}],"nextCursor":null}}"#)),
            (200,format!(r#"{{"id":"adfile:sha256:{DIGEST}","kind":"File","size":1}}"#)),
            (200,r#"{"releaseId":"release","app":{"revisionId":"a1","data":null},"release":{"revisionId":"r1","data":{"unknown":{"large":18446744073709551615}}},"associations":[{"associationId":"assoc","declaration":{"revisionId":"s1","data":[1,true,null]}}]}"#.into()),
        ]
    }
    fn server(responses: Vec<(u16, String)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buf = [0; 512];
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let reason = if status == 200 { "OK" } else { "Bad Request" };
                write!(stream,"HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (url, handle)
    }

    #[test]
    fn library_reopens_and_rejects_unknown_schema() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        let mut library = Library::open(paths.clone()).unwrap();
        let id = library.id.clone();
        let a = library
            .add_profile("One", "https://example.test", false)
            .unwrap();
        assert!(
            library
                .set_credential_ref(&a.id, Some("plaintext-token"))
                .is_err()
        );
        let b = library
            .add_profile("Two", "https://example.test", false)
            .unwrap();
        assert_ne!(a.id, b.id);
        drop(library);
        let reopened = Library::open(paths.clone()).unwrap();
        assert_eq!(reopened.id, id);
        assert_eq!(reopened.profiles(false).unwrap().len(), 2);
        reopened
            .conn
            .execute_batch("PRAGMA user_version=99")
            .unwrap();
        drop(reopened);
        assert!(matches!(
            Library::open(paths),
            Err(LibraryError::Invalid(_))
        ));
    }

    #[test]
    fn concurrent_open_migrates_version_four_once() {
        let dir = tempfile::tempdir().unwrap();
        let roots = paths(&dir);
        ensure_private_root(&roots.metadata).unwrap();
        ensure_private_root(&roots.content).unwrap();
        let id = Uuid::new_v4().to_string();
        write_marker(&roots.metadata, &id).unwrap();
        write_marker(&roots.content, &id).unwrap();
        let db = roots.metadata.join("library.sqlite");
        let old = Connection::open(&db).unwrap();
        old.execute_batch(SCHEMA).unwrap();
        old.execute_batch(MIGRATE_V2).unwrap();
        old.execute_batch(MIGRATE_V3).unwrap();
        old.execute_batch(MIGRATE_V4).unwrap();
        old.execute(
            "INSERT INTO library_meta(key,value) VALUES ('library_id',?1)",
            [&id],
        )
        .unwrap();
        drop(old);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let roots = roots.clone();
                thread::spawn(move || {
                    let lib = Library::open(roots).unwrap();
                    assert_eq!(
                        lib.conn
                            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                            .unwrap(),
                        5
                    );
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(Library::open(roots).unwrap().id, id);
    }

    #[test]
    fn origin_change_and_removal_retain_separate_cached_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let old = library
            .add_profile("One", "https://first.test", false)
            .unwrap();
        let same = library
            .add_profile("Other", "https://first.test", false)
            .unwrap();
        let next = library
            .replace_origin(&old.id, "https://second.test", false)
            .unwrap();
        assert_ne!(old.id, next.id);
        assert!(library.profile(&old.id).unwrap().archived);
        assert!(!library.profile(&same.id).unwrap().archived);
        library.archive_profile(&same.id).unwrap();
        assert!(library.profile(&same.id).unwrap().archived);
        assert_eq!(library.profiles(false).unwrap().len(), 1);
        assert_eq!(library.profiles(true).unwrap().len(), 3);
    }

    #[test]
    fn sync_is_atomic_preserves_exact_json_and_offline_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let (url, handle) = server(replies());
        let profile = library.add_profile("Fixture", &url, false).unwrap();
        let client = ApiClient::new(&url, None, false).unwrap();
        let summary = library.sync(&profile.id, &client).unwrap();
        assert_eq!(
            (
                summary.apps,
                summary.releases,
                summary.associations,
                summary.artifacts
            ),
            (1, 1, 1, 1)
        );
        handle.join().unwrap();
        let cached = library.cached_apps(&profile.id).unwrap();
        assert_eq!(
            cached[0].value.unknown["future"].to_string(),
            "18446744073709551616"
        );
        assert!(!cached[0].stale);
        assert_eq!(
            library.cached_releases(&profile.id, "app").unwrap().len(),
            1
        );
        assert_eq!(
            library
                .cached_associations(&profile.id, "release")
                .unwrap()
                .len(),
            1
        );
        let snapshot = library
            .cached_snapshot(&profile.id, "release")
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.value.release.data["unknown"]["large"].as_u64(),
            Some(u64::MAX)
        );
        assert_eq!(
            snapshot.value.associations[0].declaration.data,
            serde_json::json!([1, true, null])
        );
        drop(library);
        let mut library = Library::open(paths(&dir)).unwrap();
        assert_eq!(library.cached_apps(&profile.id).unwrap().len(), 1);
        let moved = library
            .replace_origin(&profile.id, "https://other.example.test", false)
            .unwrap();
        assert!(library.cached_apps(&moved.id).unwrap().is_empty());
        assert_eq!(library.cached_apps(&profile.id).unwrap().len(), 1);
    }

    #[test]
    fn failed_sync_cannot_delete_or_partially_replace_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let mut response_list = replies();
        response_list.extend([
            (200, CAPS.into()),
            (200, r#"{"items":[],"nextCursor":"bad"}"#.into()),
            (400, "{}".into()),
        ]);
        let (url, handle) = server(response_list);
        let profile = library.add_profile("Fixture", &url, false).unwrap();
        let client = ApiClient::new(&url, None, false).unwrap();
        library.sync(&profile.id, &client).unwrap();
        let first_sync = library.profile(&profile.id).unwrap().last_sync;
        assert!(library.sync(&profile.id, &client).is_err());
        handle.join().unwrap();
        assert_eq!(library.profile(&profile.id).unwrap().last_sync, first_sync);
        assert_eq!(library.cached_apps(&profile.id).unwrap().len(), 1);
    }

    #[test]
    fn database_error_rolls_back_entire_catalog_and_profile_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let (url, handle) = server(replies());
        let profile = library.add_profile("Fixture", &url, false).unwrap();
        library.conn.execute_batch("CREATE TRIGGER reject_release BEFORE INSERT ON releases BEGIN SELECT RAISE(ABORT,'test rollback'); END;").unwrap();
        let client = ApiClient::new(&url, None, false).unwrap();
        assert!(library.sync(&profile.id, &client).is_err());
        handle.join().unwrap();
        assert!(library.cached_apps(&profile.id).unwrap().is_empty());
        assert!(library.profile(&profile.id).unwrap().last_sync.is_none());
    }

    #[test]
    fn identical_remote_ids_remain_scoped_to_two_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let (first_url, first_handle) = server(replies());
        let (second_url, second_handle) = server(replies());
        let first = library.add_profile("First", &first_url, false).unwrap();
        let second = library.add_profile("Second", &second_url, false).unwrap();
        library
            .sync(&first.id, &ApiClient::new(&first_url, None, false).unwrap())
            .unwrap();
        library
            .sync(
                &second.id,
                &ApiClient::new(&second_url, None, false).unwrap(),
            )
            .unwrap();
        first_handle.join().unwrap();
        second_handle.join().unwrap();
        let one = library.cached_apps(&first.id).unwrap();
        let two = library.cached_apps(&second.id).unwrap();
        assert_eq!(one[0].value.id, two[0].value.id);
        assert_ne!(one[0].profile_id, two[0].profile_id);
        library.archive_profile(&first.id).unwrap();
        assert_eq!(library.cached_apps(&second.id).unwrap().len(), 1);
        assert_eq!(library.cached_apps(&first.id).unwrap().len(), 1);
    }

    #[test]
    fn completed_sync_marks_absent_cached_items_stale_without_deleting_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::open(paths(&dir)).unwrap();
        let mut response_list = replies();
        response_list.extend([
            (200, CAPS.into()),
            (200, r#"{"items":[],"nextCursor":null}"#.into()),
        ]);
        let (url, handle) = server(response_list);
        let profile = library.add_profile("Fixture", &url, false).unwrap();
        let client = ApiClient::new(&url, None, false).unwrap();
        library.sync(&profile.id, &client).unwrap();
        library.sync(&profile.id, &client).unwrap();
        handle.join().unwrap();
        let cached = library.cached_apps(&profile.id).unwrap();
        assert_eq!(cached.len(), 1);
        assert!(cached[0].stale);
    }

    #[test]
    fn unsafe_roots_and_mismatched_identity_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Library::open(LibraryPaths {
                metadata: PathBuf::from("relative"),
                content: dir.path().join("content")
            })
            .is_err()
        );
        assert!(
            Library::open(LibraryPaths {
                metadata: dir.path().join(".").join("metadata"),
                content: dir.path().join("content")
            })
            .is_err()
        );
        let first = paths(&dir);
        let library = Library::open(first.clone()).unwrap();
        let other = tempfile::tempdir().unwrap();
        let other_paths = paths(&other);
        Library::open(other_paths.clone()).unwrap();
        assert!(
            Library::open(LibraryPaths {
                metadata: first.metadata.clone(),
                content: other_paths.content
            })
            .is_err()
        );
        drop(library);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&first.metadata, dir.path().join("linked")).unwrap();
            assert!(
                Library::open(LibraryPaths {
                    metadata: dir.path().join("linked"),
                    content: first.content
                })
                .is_err()
            );
        }
    }
}
