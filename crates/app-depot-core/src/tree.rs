//! Canonical Tree Identity v1 and destination-independent inventory checks.
use crate::{library::LibraryError, models::InventoryEntry};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Invalid(message.into())
}

fn hex_digest(value: &str) -> Result<[u8; 32], LibraryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid lowercase SHA-256 digest"));
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("invalid SHA-256 digest"))?;
    }
    Ok(bytes)
}

fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= u32::MAX as usize
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains(['\\', '\0'])
        && !path
            .as_bytes()
            .get(0..2)
            .is_some_and(|b| b[0].is_ascii_alphabetic() && b[1] == b':')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Verify logical identity without applying host filesystem naming rules.
pub fn identity(entries: &[InventoryEntry]) -> Result<String, LibraryError> {
    let mut sorted: Vec<&InventoryEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.path().as_bytes().cmp(b.path().as_bytes()));
    let mut hash = Sha256::new();
    hash.update(b"APPDEPOT-TREE");
    hash.update(1u16.to_le_bytes());
    hash.update((sorted.len() as u64).to_le_bytes());
    let mut previous: Option<&[u8]> = None;
    let mut directories = HashSet::new();
    let mut file_count = 0usize;
    let mut file_ids = HashSet::new();
    for entry in sorted {
        let path = entry.path();
        if !path_valid(path) || previous.is_some_and(|prior| prior >= path.as_bytes()) {
            return Err(invalid(format!("invalid or duplicate tree path: {path:?}")));
        }
        if let Some((parent, _)) = path.rsplit_once('/')
            && !directories.contains(parent)
        {
            return Err(invalid(format!("missing parent directory for {path:?}")));
        }
        previous = Some(path.as_bytes());
        match entry {
            InventoryEntry::Directory { .. } => {
                hash.update([2]);
                directories.insert(path);
            }
            InventoryEntry::File {
                file_id,
                size,
                sha256,
                executable,
                ..
            } => {
                if uuid::Uuid::parse_str(file_id).map_or(true, |id| id.to_string() != *file_id)
                    || !file_ids.insert(file_id)
                {
                    return Err(invalid("invalid or duplicate tree file ID"));
                }
                let digest = hex_digest(sha256)?;
                hash.update([1]);
                hash.update((path.len() as u32).to_le_bytes());
                hash.update(path.as_bytes());
                hash.update(size.to_le_bytes());
                hash.update(digest);
                hash.update([u8::from(*executable)]);
                file_count += 1;
                continue;
            }
        }
        hash.update((path.len() as u32).to_le_bytes());
        hash.update(path.as_bytes());
    }
    if file_count == 0 {
        return Err(invalid("tree has no regular file"));
    }
    Ok(format!("adtree:v1:sha256:{:x}", hash.finalize()))
}

/// Reject names that can change, collide or be unrepresentable on supported destinations.
pub fn validate_destination(entries: &[InventoryEntry]) -> Result<(), LibraryError> {
    let mut folded = HashSet::new();
    for entry in entries {
        let path = entry.path();
        for component in path.split('/') {
            #[cfg(windows)]
            crate::windows_fs::validate_component(component)
                .map_err(|_| invalid(format!("destination-incompatible path: {path:?}")))?;
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if component.len() > 255
                || component.ends_with([' ', '.'])
                || component.contains(['<', '>', ':', '"', '|', '?', '*'])
                || component.chars().any(char::is_control)
                || matches!(
                    stem.as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
            {
                return Err(invalid(format!("destination-incompatible path: {path:?}")));
            }
        }
        let normalized: String = path.nfc().collect();
        let key: String = normalized.as_str().case_fold().collect();
        if !folded.insert(key) {
            return Err(invalid(format!("destination name collision: {path:?}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(path: &str, bytes: &[u8], executable: bool) -> InventoryEntry {
        InventoryEntry::File {
            path: path.into(),
            file_id: uuid::Uuid::new_v4().to_string(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            executable,
        }
    }
    fn dir(path: &str) -> InventoryEntry {
        InventoryEntry::Directory { path: path.into() }
    }
    #[test]
    fn server_positive_vectors() {
        let cases = [
            (
                vec![file("a.txt", b"A", false)],
                "49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1",
            ),
            (
                vec![
                    file("b.txt", b"B", false),
                    dir("d"),
                    file("d/a.txt", b"A", false),
                    file("d/z.bin", &[0, 255], false),
                ],
                "8270d0fb4808f9cbdc391f9c27eb83b78944716a7f838a2aab348db1e2f45645",
            ),
            (
                vec![
                    file("a.txt", b"A", false),
                    dir("empty"),
                    dir("empty/nested"),
                ],
                "de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f",
            ),
            (
                vec![file("a.txt", b"A", true)],
                "082f91792688612638863968a8f5923a0d78672c1c705f0c15c769c4146c2c65",
            ),
            (
                vec![file("A.txt", b"A", false), file("a.txt", b"A", false)],
                "d0f7a0fbb72d807764a41fbf871e29d1f42eb5f663af6d9c19a10e61a98899db",
            ),
            (
                vec![
                    file("cafe\u{301}.txt", b"A", false),
                    file("café.txt", b"A", false),
                ],
                "c1f6b66d1bb2cd88636493b65469c2eee6d01f9ba3c539e6936e75679d5a6f7b",
            ),
            (
                vec![file("a.txt", b"B", false)],
                "8dc2adf5183190576bf7c0142cfc8fdb6f70fc2a01c115a33e9012b10213e42b",
            ),
        ];
        for (entries, expected) in cases {
            assert_eq!(
                identity(&entries).unwrap(),
                format!("adtree:v1:sha256:{expected}")
            );
        }
    }
    #[test]
    fn logical_and_destination_errors_are_distinct() {
        let cases = [
            vec![file("../x", b"A", false)],
            vec![file("a\\b", b"A", false)],
            vec![file("a/b", b"A", false)],
            vec![file("a", b"A", false), file("a", b"B", false)],
        ];
        for entries in cases {
            assert!(identity(&entries).is_err());
        }
        let case = vec![file("A.txt", b"A", false), file("a.txt", b"A", false)];
        assert!(identity(&case).is_ok());
        assert!(validate_destination(&case).is_err());
        let unicode = vec![
            file("cafe\u{301}.txt", b"A", false),
            file("café.txt", b"A", false),
        ];
        assert!(identity(&unicode).is_ok());
        assert!(validate_destination(&unicode).is_err());
        let case_fold = vec![
            file("straße.txt", b"A", false),
            file("strasse.txt", b"A", false),
        ];
        assert!(identity(&case_fold).is_ok());
        assert!(validate_destination(&case_fold).is_err());
        for name in ["CON.txt", "x.", "x ", "dir:x", "a?b"] {
            let entries = vec![file(name, b"A", false)];
            assert!(identity(&entries).is_ok());
            assert!(validate_destination(&entries).is_err());
        }
    }

    #[test]
    fn server_logical_negative_vectors() {
        // N8-N12 are representable as inventory objects and must fail before identity acceptance.
        let cases = [
            vec![file("a.txt", b"A", false), file("d/a.txt", b"A", false)],
            vec![file("d", b"A", false), file("d/a.txt", b"A", false)],
            vec![file("a.txt", b"A", false), file("a.txt", b"B", false)],
            vec![dir("a.txt"), file("a.txt", b"A", false)],
            vec![dir("empty")],
        ];
        for entries in cases {
            assert!(identity(&entries).is_err());
        }
        assert!(
            serde_json::from_str::<InventoryEntry>(r#"{"type":"SymbolicLink","path":"link"}"#)
                .is_err()
        );
    }
}
