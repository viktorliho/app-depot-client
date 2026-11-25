//! Filesystem boundary for a validated Standard installation layout.
use crate::{installation::Plan, library::LibraryError, models::InventoryEntry};

/// Final destination inventory, not an Artifact inventory or a Tree identity.
/// Source file IDs may repeat; destination paths must be unique.
pub fn destination_layout(plan: &Plan) -> Result<Vec<InventoryEntry>, LibraryError> {
    let (Some(files), Some(directories)) = (&plan.files, &plan.directories) else {
        return Err(LibraryError::Invalid(
            "Derived installation is unsupported".into(),
        ));
    };
    let mut entries = Vec::with_capacity(files.len() + directories.len());
    entries.extend(
        directories
            .iter()
            .map(|path| InventoryEntry::Directory { path: path.clone() }),
    );
    entries.extend(files.iter().map(|(path, source)| InventoryEntry::File {
        path: path.clone(),
        file_id: source.file_id.clone().unwrap_or_default(),
        size: source.size,
        sha256: source.sha256.clone(),
        executable: source.executable,
    }));
    crate::tree::validate_destination(&entries)?;
    entries.sort_unstable_by(|a, b| a.path().cmp(b.path()));
    Ok(entries)
}

#[cfg(target_os = "linux")]
pub(crate) fn publish_generation(
    content: &std::path::Path,
    id: &str,
    entries: &[InventoryEntry],
    object: impl Fn(&str) -> Result<std::path::PathBuf, LibraryError>,
) -> Result<(), LibraryError> {
    let stage = content.join("staging").join(format!("manifest-{id}"));
    let target = content.join("apps").join(id);
    publish_layout(content, &stage, &target, id, entries, object)
}

#[cfg(target_os = "linux")]
fn publish_layout(
    content: &std::path::Path,
    stage: &std::path::Path,
    target: &std::path::Path,
    id: &str,
    entries: &[InventoryEntry],
    object: impl Fn(&str) -> Result<std::path::PathBuf, LibraryError>,
) -> Result<(), LibraryError> {
    use crate::{
        managed::copy_independent,
        transfer::{private_dir, sync_dir},
        tree_download::{
            create_file_beneath, dir_beneath, open_dir_nofollow, rename_tree_noreplace,
        },
    };
    use std::io::Write;
    // Stage belongs to a durable generation or attempt; incomplete work is reproducible.
    let parent = stage
        .parent()
        .ok_or_else(|| LibraryError::Invalid("layout stage has no parent".into()))?;
    let name = stage
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| LibraryError::Invalid("invalid layout stage name".into()))?;
    crate::managed::delete_owned(content, parent, name)?;
    private_dir(stage)?;
    let root = open_dir_nofollow(stage)?;
    let payload = dir_beneath(&root, "payload", true)?;
    // Original Tree inventories need not arrive in parent-before-child order.
    let mut ordered: Vec<_> = entries.iter().collect();
    ordered.sort_unstable_by(|left, right| left.path().cmp(right.path()));
    for entry in &ordered {
        match entry {
            InventoryEntry::Directory { path } => {
                dir_beneath(&payload, path, true)?;
            }
            InventoryEntry::File {
                path,
                size,
                sha256,
                executable,
                ..
            } => {
                let mut output = create_file_beneath(&payload, path)?;
                copy_independent(
                    &object(sha256)?,
                    &mut output,
                    &stage.join("payload").join(path),
                    *size,
                    sha256,
                    *executable,
                )?;
            }
        }
    }
    for entry in ordered.into_iter().rev() {
        if let InventoryEntry::Directory { path } = entry {
            dir_beneath(&payload, path, false)?.sync_all()?;
        }
    }
    payload.sync_all()?;
    let mut marker = create_file_beneath(&root, ".app-depot-installation-id")?;
    marker.write_all(id.as_bytes())?;
    marker.sync_all()?;
    root.sync_all()?;
    verify_generation(stage, id, entries)?;
    rename_tree_noreplace(stage, target)?;
    sync_dir(
        target
            .parent()
            .ok_or_else(|| LibraryError::Invalid("layout target has no parent".into()))?,
    )?;
    sync_dir(parent)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn verify_generation(
    root: &std::path::Path,
    id: &str,
    entries: &[InventoryEntry],
) -> Result<(), LibraryError> {
    use crate::{
        launch::open_nofollow,
        managed::{hash_open, runtime_paths},
    };
    use std::{collections::BTreeSet, os::unix::fs::PermissionsExt};
    verify_linux_marker(root, id)?;
    open_nofollow(root, "payload", true)?;
    let mut expected = BTreeSet::from([
        ".app-depot-installation-id".to_owned(),
        "payload".to_owned(),
    ]);
    for entry in entries {
        let relative = format!("payload/{}", entry.path());
        expected.insert(relative.clone());
        match entry {
            InventoryEntry::Directory { .. } => {
                open_nofollow(root, &relative, true)?;
            }
            InventoryEntry::File {
                size,
                sha256,
                executable,
                ..
            } => {
                let file = open_nofollow(root, &relative, false)?;
                let meta = file.metadata()?;
                if !meta.is_file()
                    || meta.len() != *size
                    || (meta.permissions().mode() & 0o111 != 0) != *executable
                    || hash_open(file)? != *sha256
                {
                    return Err(LibraryError::Invalid(format!(
                        "installation file differs: {relative}"
                    )));
                }
            }
        }
    }
    if runtime_paths(root)? != expected {
        return Err(LibraryError::Invalid("installation layout differs".into()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn verify_linux_marker(root: &std::path::Path, id: &str) -> Result<(), LibraryError> {
    use std::io::Read;
    let marker = crate::launch::open_nofollow(root, ".app-depot-installation-id", false)?;
    if !marker.metadata()?.is_file() || marker.metadata()?.len() != 36 {
        return Err(LibraryError::Invalid(
            "invalid installation identity marker".into(),
        ));
    }
    let mut value = String::new();
    marker.take(37).read_to_string(&mut value)?;
    if value != id {
        return Err(LibraryError::Invalid(
            "installation identity mismatch".into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn publish_generation(
    content: &std::path::Path,
    id: &str,
    entries: &[InventoryEntry],
    object: impl Fn(&str) -> Result<std::path::PathBuf, LibraryError>,
) -> Result<(), LibraryError> {
    crate::windows_fs::publish_tree_install(
        &content.join("apps").join(id),
        id,
        entries,
        |digest| object(digest).map_err(std::io::Error::other),
    )?;
    Ok(())
}
#[cfg(windows)]
pub(crate) fn verify_generation(
    root: &std::path::Path,
    id: &str,
    entries: &[InventoryEntry],
) -> Result<(), LibraryError> {
    crate::windows_fs::verify_tree_install(root, id, entries)?;
    Ok(())
}

/// Durable owner is the prerequisite attempt row, not the generation payload.
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn prerequisite_workspace(
    content: &std::path::Path,
    attempt: &str,
) -> Result<std::path::PathBuf, LibraryError> {
    if !uuid::Uuid::parse_str(attempt).is_ok_and(|id| id.to_string() == attempt) {
        return Err(LibraryError::Invalid(
            "invalid prerequisite attempt identity".into(),
        ));
    }
    Ok(content
        .join("staging")
        .join(format!("prerequisite-{attempt}")))
}

#[cfg(any(target_os = "linux", windows))]
pub(crate) fn publish_prerequisite_source(
    content: &std::path::Path,
    attempt: &str,
    entries: &[InventoryEntry],
    relative_file: &str,
    object: impl Fn(&str) -> Result<std::path::PathBuf, LibraryError>,
) -> Result<crate::prerequisite::PreparedPrerequisiteSource, LibraryError> {
    crate::tree::validate_destination(entries)?;
    if !entries
        .iter()
        .any(|e| matches!(e, InventoryEntry::File { path, .. } if path == relative_file))
    {
        return Err(LibraryError::Invalid(
            "declared prerequisite file missing from source layout".into(),
        ));
    }
    let workspace = prerequisite_workspace(content, attempt)?;
    // Exclusive creation, with a durable owner marker before materialization.
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, io::Write, os::fd::AsRawFd};
        let parent = crate::launch::open_nofollow(content, "staging", true)?;
        let name = format!("prerequisite-{attempt}");
        let c_name = CString::new(name.as_str())
            .map_err(|_| LibraryError::Invalid("invalid workspace name".into()))?;
        // SAFETY: parent is a live, no-follow directory handle and c_name is NUL terminated.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let owned = crate::tree_download::dir_beneath(&parent, &name, false)?;
        let mut marker =
            crate::tree_download::create_file_beneath(&owned, ".app-depot-prerequisite-id")?;
        marker.write_all(attempt.as_bytes())?;
        marker.sync_all()?;
        owned.sync_all()?;
        parent.sync_all()?;
    }
    #[cfg(windows)]
    crate::windows_fs::create_prerequisite_workspace(&workspace, attempt)?;
    let owned = workspace.join(attempt);
    #[cfg(target_os = "linux")]
    publish_layout(
        content,
        &workspace.join("partial"),
        &owned,
        attempt,
        entries,
        object,
    )?;
    #[cfg(windows)]
    crate::windows_fs::publish_tree_install(&owned, attempt, entries, |digest| {
        object(digest).map_err(std::io::Error::other)
    })?;
    verify_generation(&owned, attempt, entries)?;
    let root = owned.join("payload");
    let file = root.join(relative_file);
    let directory = file
        .parent()
        .ok_or_else(|| LibraryError::Invalid("prerequisite file has no directory".into()))?
        .to_path_buf();
    Ok(crate::prerequisite::PreparedPrerequisiteSource {
        root,
        file,
        directory,
    })
}

/// Remove only a journal-owned workspace whose executor is known to be idle.
/// An interrupted or ambiguous execution must be retained until explicitly
/// reconciled by the platform lifecycle.
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn cleanup_prerequisite_workspace(
    content: &std::path::Path,
    attempt: &str,
) -> Result<(), LibraryError> {
    let workspace = prerequisite_workspace(content, attempt)?;
    match std::fs::symlink_metadata(&workspace) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let marker = crate::launch::open_nofollow(&workspace, ".app-depot-prerequisite-id", false)?;
        let metadata = marker.metadata()?;
        if !metadata.is_file() || metadata.len() != 36 {
            return Err(LibraryError::Invalid(
                "invalid prerequisite workspace ownership marker".into(),
            ));
        }
        let mut owner = String::new();
        marker.take(37).read_to_string(&mut owner)?;
        if owner != attempt {
            return Err(LibraryError::Invalid(
                "prerequisite workspace ownership marker mismatch".into(),
            ));
        }
        for entry in std::fs::read_dir(&workspace)? {
            let name = entry?.file_name();
            if name == ".app-depot-prerequisite-id" {
                continue;
            }
            let name = name.to_str().ok_or_else(|| {
                LibraryError::Invalid("non-Unicode prerequisite workspace entry".into())
            })?;
            crate::managed::delete_owned(content, &workspace, name)?;
        }
        crate::managed::delete_owned(
            content,
            &content.join("staging"),
            &format!("prerequisite-{attempt}"),
        )?;
    }
    #[cfg(windows)]
    crate::windows_fs::cleanup_prerequisite_workspace(&workspace, attempt)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn destination_projection_preserves_duplicate_sources_and_empty_directories() {
        let source = json!({"associationId":"11111111-1111-1111-1111-111111111111", "artifactId":format!("adfile:sha256:{}", "a".repeat(64)),"fileId":null,"size":2,"sha256":"a".repeat(64),"executable":true});
        let plan: Plan = serde_json::from_value(json!({"version":1,"appId":"a","releaseId":"r","revisionId":"v","platform":"linux/x86_64","selections":{},"unresolved":[],"files":{"bin/renamed":source,"copy":source},"directories":["bin","empty"],"prerequisites":[],"launch":null})).unwrap();
        let layout = destination_layout(&plan).unwrap();
        assert_eq!(
            layout.iter().map(InventoryEntry::path).collect::<Vec<_>>(),
            ["bin", "bin/renamed", "copy", "empty"]
        );
        assert!(matches!(
            &layout[1],
            InventoryEntry::File {
                executable: true,
                size: 2,
                ..
            }
        ));
    }
}
