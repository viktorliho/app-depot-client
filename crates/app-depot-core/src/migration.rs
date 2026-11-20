//! Explicit, source-preserving migration of a closed local library to one root.
//! The old library is never removed by this operation.
use crate::library::{
    Library, LibraryError, LibraryPaths, access_lock, ensure_directory, read_marker, write_marker,
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationEstimate {
    pub source_logical_bytes: u64,
    pub destination_available_bytes: u64,
    pub destination: PathBuf,
}

fn sum_regular_bytes(root: &Path) -> Result<u64, LibraryError> {
    let mut total = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        let bytes = if metadata.is_dir() && !metadata.file_type().is_symlink() {
            sum_regular_bytes(&entry.path())?
        } else if metadata.is_file() && !metadata.file_type().is_symlink() {
            metadata.len()
        } else {
            0
        };
        total = total
            .checked_add(bytes)
            .ok_or_else(|| invalid("migration size overflow"))?;
    }
    Ok(total)
}

#[cfg(target_os = "linux")]
pub fn migration_estimate(
    source: &LibraryPaths,
    destination: &Path,
) -> Result<MigrationEstimate, LibraryError> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("migration destination has no parent"))?;
    let parent = crate::launch::open_nofollow(parent, "", true)?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    use std::os::fd::AsRawFd;
    if unsafe { libc::fstatvfs(parent.as_raw_fd(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut logical = sum_regular_bytes(&source.metadata)?;
    if source.content != source.metadata {
        logical = logical
            .checked_add(sum_regular_bytes(&source.content)?)
            .ok_or_else(|| invalid("migration size overflow"))?;
    }
    Ok(MigrationEstimate {
        source_logical_bytes: logical,
        destination_available_bytes: stats
            .f_bavail
            .checked_mul(stats.f_frsize)
            .ok_or_else(|| invalid("available space overflow"))?,
        destination: destination.to_path_buf(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn migration_estimate(
    _source: &LibraryPaths,
    _destination: &Path,
) -> Result<MigrationEstimate, LibraryError> {
    Err(invalid(
        "library migration is currently available only on Linux",
    ))
}

#[cfg(target_os = "linux")]
fn regular_nofollow(path: &Path) -> Result<File, LibraryError> {
    let file = crate::launch::open_nofollow(path, "", false)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("migration source contains a non-regular file"));
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn mount_id(file: &File) -> Result<u64, LibraryError> {
    use std::os::fd::AsRawFd;
    let empty = std::ffi::CString::new("").expect("literal");
    let mut result: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty.as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            &mut result,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if result.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(invalid("filesystem cannot prove migration mount boundary"));
    }
    Ok(result.stx_mnt_id)
}

#[cfg(target_os = "linux")]
fn copy_file(source: &Path, destination: &Path) -> Result<u64, LibraryError> {
    use sha2::{Digest, Sha256};
    use std::os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let mut input = regular_nofollow(source)?;
    let metadata = input.metadata()?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(destination)?;
    let cloned =
        unsafe { libc::ioctl(output.as_raw_fd(), 0x4004_9409_u64, input.as_raw_fd()) } == 0;
    if !cloned {
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP | libc::EXDEV | libc::ENOTTY | libc::EINVAL)
        ) {
            return Err(error.into());
        }
        std::io::copy(&mut input, &mut output)?;
    }
    output.set_permissions(fs::Permissions::from_mode(
        metadata.permissions().mode() & 0o700,
    ))?;
    output.sync_all()?;
    let mut source_hash = Sha256::new();
    let mut destination_hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut read = regular_nofollow(source)?;
    loop {
        let n = read.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        source_hash.update(&buffer[..n]);
    }
    let mut read = regular_nofollow(destination)?;
    loop {
        let n = read.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        destination_hash.update(&buffer[..n]);
    }
    if source_hash.finalize() != destination_hash.finalize()
        || fs::metadata(destination)?.len() != metadata.len()
    {
        return Err(invalid("migration copy changed during verification"));
    }
    Ok(metadata.len())
}

#[cfg(target_os = "linux")]
fn copy_dir(
    source: &Path,
    destination: &Path,
    root_mount: u64,
    metadata_root: bool,
) -> Result<u64, LibraryError> {
    use std::os::unix::fs::PermissionsExt;
    let src = crate::launch::open_nofollow(source, "", true)?;
    if mount_id(&src)? != root_mount {
        return Err(invalid("migration source crosses a filesystem boundary"));
    }
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    let mut bytes = 0u64;
    for item in fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        if name == OsStr::new(".app-depot-library-id") {
            continue;
        }
        if metadata_root
            && ["library.sqlite", "library.sqlite-wal", "library.sqlite-shm"]
                .iter()
                .any(|s| name == OsStr::new(s))
        {
            continue;
        }
        let src_path = item.path();
        let dst_path = destination.join(&name);
        if fs::symlink_metadata(&dst_path).is_ok() {
            return Err(invalid("migration roots contain conflicting paths"));
        }
        let metadata = fs::symlink_metadata(&src_path)?;
        if metadata.is_dir()
            && mount_id(&crate::launch::open_nofollow(&src_path, "", true)?)? != root_mount
        {
            return Err(invalid("migration source contains a nested mount"));
        }
        if metadata.is_file() && mount_id(&regular_nofollow(&src_path)?)? != root_mount {
            return Err(invalid("migration source contains a nested mount"));
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            bytes = bytes
                .checked_add(copy_dir(&src_path, &dst_path, root_mount, false)?)
                .ok_or_else(|| invalid("migration size overflow"))?;
        } else if metadata.is_file() && !metadata.file_type().is_symlink() {
            bytes = bytes
                .checked_add(copy_file(&src_path, &dst_path)?)
                .ok_or_else(|| invalid("migration size overflow"))?;
        } else if metadata.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src_path)?, &dst_path)?;
        } else {
            return Err(invalid(format!(
                "migration source contains an unsafe entry: {}",
                src_path.display()
            )));
        }
    }
    File::open(destination)?.sync_all()?;
    Ok(bytes)
}

/// Copy a closed library into a new single-root location. The source remains intact.
/// A failed or interrupted attempt leaves only an explicitly named staging directory.
#[cfg(target_os = "linux")]
pub fn migrate_library(source: &LibraryPaths, destination: &Path) -> Result<PathBuf, LibraryError> {
    use std::os::unix::fs::PermissionsExt;
    if !destination.is_absolute()
        || destination == Path::new("/")
        || source.metadata == destination
        || source.content == destination
        || destination.starts_with(&source.metadata)
        || destination.starts_with(&source.content)
        || source.metadata.starts_with(destination)
        || source.content.starts_with(destination)
    {
        return Err(invalid("source and destination library roots overlap"));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(invalid("migration destination already exists"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("migration destination has no parent"))?;
    ensure_directory(parent)?;
    let metadata_id = read_marker(&source.metadata)?
        .ok_or_else(|| invalid("source metadata root lacks library identity"))?;
    let content_id = read_marker(&source.content)?
        .ok_or_else(|| invalid("source content root lacks library identity"))?;
    if metadata_id != content_id {
        return Err(invalid("source roots belong to different libraries"));
    }
    // Every current client Library handle holds a shared lock. Refuse migration while
    // another client can change the source database or download files.
    let _source_lock = access_lock(&source.metadata, true)?;
    let database = source.metadata.join("library.sqlite");
    let _file = regular_nofollow(&database)?;
    let estimate = migration_estimate(source, destination)?;
    let metadata_min = fs::metadata(&database)?
        .len()
        .saturating_add(16 * 1024 * 1024);
    if estimate.destination_available_bytes < metadata_min {
        return Err(invalid(
            "destination has insufficient free space for a safe SQLite copy",
        ));
    }
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(invalid("source SQLite integrity check failed"));
    }
    let active:i64=connection.query_row("SELECT count(*) FROM download_jobs WHERE state IN ('Queued','FetchingInventory','Downloading','Verifying','Committing')",[],|r|r.get(0))?;
    if active != 0 {
        return Err(invalid("pause active downloads before migrating"));
    }
    let stage = parent.join(format!(".appdepot-migrating-{}", Uuid::new_v4()));
    fs::create_dir(&stage)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
    write_marker(&stage, &metadata_id)?;
    let content_mount = mount_id(&crate::launch::open_nofollow(&source.content, "", true)?)?;
    let metadata_mount = mount_id(&crate::launch::open_nofollow(&source.metadata, "", true)?)?;
    // Copy content first. A regular unexpected file inside a legacy materialization is
    // preserved but is never promoted as verified by the destination library.
    for item in fs::read_dir(&source.content)? {
        let item = item?;
        let name = item.file_name();
        if name == OsStr::new(".app-depot-library-id")
            || (source.content == source.metadata
                && ["library.sqlite", "library.sqlite-wal", "library.sqlite-shm"]
                    .iter()
                    .any(|s| name == OsStr::new(s)))
        {
            continue;
        }
        let src = item.path();
        let dst = stage.join(&name);
        let metadata = fs::symlink_metadata(&src)?;
        if metadata.is_dir()
            && mount_id(&crate::launch::open_nofollow(&src, "", true)?)? != content_mount
        {
            return Err(invalid("source contains nested mount"));
        }
        if metadata.is_file() && mount_id(&regular_nofollow(&src)?)? != content_mount {
            return Err(invalid("source contains nested mount"));
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            copy_dir(&src, &dst, content_mount, false)?;
        } else if metadata.is_file() && !metadata.file_type().is_symlink() {
            copy_file(&src, &dst)?;
        } else if metadata.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else {
            return Err(invalid("unsafe content-root entry"));
        }
    }
    if source.metadata != source.content {
        for item in fs::read_dir(&source.metadata)? {
            let item = item?;
            let name = item.file_name();
            if name == OsStr::new(".app-depot-library-id")
                || ["library.sqlite", "library.sqlite-wal", "library.sqlite-shm"]
                    .iter()
                    .any(|s| name == OsStr::new(s))
            {
                continue;
            }
            let src = item.path();
            let dst = stage.join(&name);
            if fs::symlink_metadata(&dst).is_ok() {
                return Err(invalid("source roots contain conflicting entries"));
            }
            let metadata = fs::symlink_metadata(&src)?;
            if metadata.is_dir()
                && mount_id(&crate::launch::open_nofollow(&src, "", true)?)? != metadata_mount
            {
                return Err(invalid("source metadata contains nested mount"));
            }
            if metadata.is_file() && mount_id(&regular_nofollow(&src)?)? != metadata_mount {
                return Err(invalid("source metadata contains nested mount"));
            }
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                copy_dir(&src, &dst, metadata_mount, false)?;
            } else if metadata.is_file() && !metadata.file_type().is_symlink() {
                copy_file(&src, &dst)?;
            } else if metadata.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
            } else {
                return Err(invalid("unsafe metadata-root entry"));
            }
        }
    }
    let backup = stage.join("library.sqlite");
    connection.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    File::open(&backup)?.sync_all()?;
    let new = Library::open(LibraryPaths {
        metadata: stage.clone(),
        content: stage.clone(),
    })?;
    let copied_id = new.id.clone();
    drop(new);
    if copied_id != metadata_id {
        return Err(invalid("destination library identity changed"));
    }
    File::open(&stage)?.sync_all()?;
    crate::tree_download::rename_tree_noreplace(&stage, destination)?;
    File::open(parent)?.sync_all()?;
    Ok(destination.to_path_buf())
}

#[cfg(not(target_os = "linux"))]
pub fn migrate_library(
    _source: &LibraryPaths,
    _destination: &Path,
) -> Result<PathBuf, LibraryError> {
    Err(invalid(
        "library migration is currently available only on Linux",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn migrates_separate_roots_and_preserves_unexpected_runtime_file() {
        let temp = tempfile::tempdir().unwrap();
        let source = LibraryPaths {
            metadata: temp.path().join("old-meta"),
            content: temp.path().join("old-content"),
        };
        let lib = Library::open(source.clone()).unwrap();
        let id = lib.id.clone();
        fs::write(source.metadata.join("desktop-preferences.json"), b"{}").unwrap();
        let directory = source.content.join("artifacts").join("legacy-extra");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("23.log"), b"generated at runtime").unwrap();
        drop(lib);
        let destination = temp.path().join(".appdepot");
        migrate_library(&source, &destination).unwrap();
        let migrated = Library::open(LibraryPaths {
            metadata: destination.clone(),
            content: destination.clone(),
        })
        .unwrap();
        assert_eq!(migrated.id, id);
        assert_eq!(
            fs::read(destination.join("artifacts/legacy-extra/23.log")).unwrap(),
            b"generated at runtime"
        );
        assert_eq!(
            fs::read(source.content.join("artifacts/legacy-extra/23.log")).unwrap(),
            b"generated at runtime"
        );
        assert!(source.metadata.join("library.sqlite").exists());
    }

    #[test]
    fn failed_or_interrupted_migration_keeps_source_and_refuses_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = LibraryPaths {
            metadata: temp.path().join("old"),
            content: temp.path().join("old"),
        };
        let lib = Library::open(source.clone()).unwrap();
        drop(lib);
        let destination = temp.path().join("new");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), b"user data").unwrap();
        assert!(migrate_library(&source, &destination).is_err());
        assert_eq!(fs::read(destination.join("keep")).unwrap(), b"user data");
        assert!(source.metadata.join("library.sqlite").exists());
    }

    #[test]
    fn migration_refuses_open_source_library() {
        let temp = tempfile::tempdir().unwrap();
        let source = LibraryPaths {
            metadata: temp.path().join("old"),
            content: temp.path().join("old"),
        };
        let lib = Library::open(source.clone()).unwrap();
        let destination = temp.path().join("new");
        assert!(migrate_library(&source, &destination).is_err());
        assert!(!destination.exists());
        drop(lib);
        migrate_library(&source, &destination).unwrap();
        assert!(destination.join("library.sqlite").exists());
    }

    #[test]
    fn migration_child() {
        let Some(root) = std::env::var_os("APP_DEPOT_MIGRATE_CHILD") else {
            return;
        };
        let root = PathBuf::from(root);
        let source = LibraryPaths {
            metadata: root.join("old"),
            content: root.join("old"),
        };
        migrate_library(&source, &root.join("new")).unwrap();
    }

    #[test]
    fn sigkill_during_migration_preserves_source_and_retry_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let source = LibraryPaths {
            metadata: temp.path().join("old"),
            content: temp.path().join("old"),
        };
        let lib = Library::open(source.clone()).unwrap();
        let id = lib.id.clone();
        drop(lib);
        let tree = source.content.join("artifacts").join("legacy-extra");
        fs::create_dir(&tree).unwrap();
        for n in 0..2_000 {
            fs::write(tree.join(format!("file-{n:04}")), b"preserve").unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "migration::tests::migration_child"])
            .env("APP_DEPOT_MIGRATE_CHILD", temp.path())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let mut observed = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            for entry in fs::read_dir(temp.path()).unwrap() {
                let entry = entry.unwrap();
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".appdepot-migrating-")
                    && entry.path().join("artifacts").exists()
                {
                    observed = true;
                    break;
                }
            }
            if observed {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(observed, "migration did not expose its staging directory");
        unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        assert!(!child.wait().unwrap().success());
        assert!(source.metadata.join("library.sqlite").exists());
        assert_eq!(fs::read(tree.join("file-0000")).unwrap(), b"preserve");
        let destination = temp.path().join("new");
        migrate_library(&source, &destination).unwrap();
        let migrated = Library::open(LibraryPaths {
            metadata: destination.clone(),
            content: destination.clone(),
        })
        .unwrap();
        assert_eq!(migrated.id, id);
        assert_eq!(
            fs::read(destination.join("artifacts/legacy-extra/file-1999")).unwrap(),
            b"preserve"
        );
    }
}
