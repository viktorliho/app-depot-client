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
    use crate::{
        managed::copy_independent,
        transfer::{private_dir, sync_dir},
        tree_download::{
            create_file_beneath, dir_beneath, open_dir_nofollow, rename_tree_noreplace,
        },
    };
    use std::io::Write;
    let stage = content.join("staging").join(format!("manifest-{id}"));
    let target = content.join("apps").join(id);
    // Stage belongs to a durable generation; incomplete work is reproducible.
    crate::managed::delete_owned(content, &content.join("staging"), &format!("manifest-{id}"))?;
    private_dir(&stage)?;
    let root = open_dir_nofollow(&stage)?;
    let payload = dir_beneath(&root, "payload", true)?;
    for entry in entries {
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
    for entry in entries.iter().rev() {
        if let InventoryEntry::Directory { path } = entry {
            dir_beneath(&payload, path, false)?.sync_all()?;
        }
    }
    payload.sync_all()?;
    let mut marker = create_file_beneath(&root, ".app-depot-installation-id")?;
    marker.write_all(id.as_bytes())?;
    marker.sync_all()?;
    root.sync_all()?;
    verify_generation(&stage, id, entries)?;
    rename_tree_noreplace(&stage, &target)?;
    sync_dir(&content.join("apps"))?;
    sync_dir(&content.join("staging"))?;
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
