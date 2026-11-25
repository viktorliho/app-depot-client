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
