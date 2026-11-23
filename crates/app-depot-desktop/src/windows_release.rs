use super::{
    MANIFEST, MAX_IMAGE, Release, ResponseKind, SETUP, Version, asset, discover_desktop_release,
    hex_digest, read_bounded, request, stream_verified, validate_response_kind,
};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

fn validate_manifest(
    bytes: &[u8],
    asset_size: u64,
    asset_hash: &str,
    setup_hash: &str,
) -> Result<(), String> {
    if bytes.len() as u64 != asset_size || format!("{:x}", Sha256::digest(bytes)) != asset_hash {
        return Err("Checksum manifest differs from GitHub release metadata".into());
    }
    let contents = std::str::from_utf8(bytes).map_err(|_| "Checksum manifest is not UTF-8")?;
    let mut setup = None;
    for line in contents.lines() {
        let (hash, name) = line.split_once("  ").ok_or("Invalid checksum manifest")?;
        if !hex_digest(hash) || name.is_empty() {
            return Err("Invalid checksum manifest".into());
        }
        if name == SETUP && setup.replace(hash).is_some() {
            return Err("Duplicate Windows installer checksum".into());
        }
    }
    if !setup.is_some_and(|value| value.eq_ignore_ascii_case(setup_hash)) {
        return Err("GitHub installer digest and checksum manifest disagree".into());
    }
    Ok(())
}

pub(super) fn check() -> Result<Option<Release>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok_or("Invalid running version")?;
    let Some(release) = discover_desktop_release(current)? else {
        return Ok(None);
    };
    let (setup, setup_hash) = asset(&release, SETUP, MAX_IMAGE)?;
    let (manifest, manifest_hash) = asset(&release, MANIFEST, 4096)?;
    let bytes = read_bounded(
        &manifest.browser_download_url,
        manifest.size,
        ResponseKind::Checksum,
    )?;
    validate_manifest(&bytes, manifest.size, &manifest_hash, &setup_hash)?;
    Ok(Some(Release {
        image_size: setup.size,
        image_url: setup.browser_download_url.clone(),
        sha256: setup_hash,
        tag: release.tag_name,
        notes: release
            .body
            .unwrap_or_default()
            .chars()
            .take(20_000)
            .collect(),
    }))
}

#[derive(Debug)]
pub(crate) struct VerifiedInstaller {
    directory: tempfile::TempDir,
}

impl VerifiedInstaller {
    pub fn path(&self) -> PathBuf {
        self.directory.path().join(SETUP)
    }
}

struct Cancellable<'a, R> {
    reader: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Update download cancelled"));
        }
        self.reader.read(buffer)
    }
}

fn verify_pe(reader: &mut (impl Read + Seek), size: u64) -> io::Result<()> {
    let mut dos = [0; 64];
    reader.rewind()?;
    reader.read_exact(&mut dos)?;
    let pe_offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    if &dos[..2] != b"MZ" || pe_offset < 64 || pe_offset + 24 > size {
        return Err(io::Error::other(
            "Downloaded package has an invalid Windows executable header",
        ));
    }
    reader.seek(SeekFrom::Start(pe_offset))?;
    let mut signature = [0; 4];
    reader.read_exact(&mut signature)?;
    if &signature != b"PE\0\0" {
        return Err(io::Error::other(
            "Downloaded package has an invalid Windows executable signature",
        ));
    }
    Ok(())
}

pub(super) fn download(
    release: &Release,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<VerifiedInstaller, String> {
    let response = request(&release.image_url)?;
    validate_response_kind(&response, ResponseKind::WindowsInstaller)?;
    if let Some(length) = response.header("Content-Length")
        && length.parse::<u64>().ok() != Some(release.image_size)
    {
        return Err("Installer response size differs from GitHub release metadata".into());
    }
    stage(response.into_reader(), release, cancel, &mut progress)
}

fn stage(
    reader: impl Read,
    release: &Release,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<VerifiedInstaller, String> {
    let directory = tempfile::Builder::new()
        .prefix("app-depot-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(directory.path()).map_err(|e| e.to_string())?;
    let mut reader = Cancellable { reader, cancel };
    let mut last_report = 0;
    stream_verified(
        &mut reader,
        file.as_file_mut(),
        release.image_size,
        &release.sha256,
        |count| {
            if count.saturating_sub(last_report) >= 1024 * 1024 || count == release.image_size {
                progress(count);
                last_report = count;
            }
        },
    )?;
    verify_pe(file.as_file_mut(), release.image_size).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    let installer = VerifiedInstaller { directory };
    file.persist_noclobber(installer.path())
        .map_err(|e| e.to_string())?;
    Ok(installer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{ApiAsset, ApiRelease, expected_url};

    fn release(tag: &str, draft: bool, prerelease: bool) -> ApiRelease {
        ApiRelease {
            tag_name: tag.into(),
            draft,
            prerelease,
            body: None,
            assets: Vec::new(),
        }
    }

    #[test]
    fn requires_exact_asset_url_and_agreeing_manifest() {
        let hash = "a".repeat(64);
        let mut item = release("v1.0.0", false, false);
        item.assets.push(ApiAsset {
            name: SETUP.into(),
            size: 100,
            browser_download_url: expected_url(&item.tag_name, SETUP),
            digest: Some(format!("sha256:{hash}")),
        });
        assert_eq!(asset(&item, SETUP, 100).unwrap().1, hash);
        item.assets[0].browser_download_url.push_str("?other=1");
        assert!(asset(&item, SETUP, 100).is_err());
        let text = format!("{hash}  {SETUP}\r\n");
        let bytes = text.as_bytes();
        let manifest_hash = format!("{:x}", Sha256::digest(bytes));
        assert!(validate_manifest(bytes, bytes.len() as u64, &manifest_hash, &hash).is_ok());
        assert!(validate_manifest(bytes, bytes.len() as u64, &hash, &hash).is_err());
        assert!(
            validate_manifest(bytes, bytes.len() as u64, &manifest_hash, &"b".repeat(64)).is_err()
        );
        let duplicate = text.repeat(2);
        let bytes = duplicate.as_bytes();
        assert!(
            validate_manifest(
                bytes,
                bytes.len() as u64,
                &format!("{:x}", Sha256::digest(bytes)),
                &hash
            )
            .is_err()
        );
    }

    #[test]
    fn stages_only_complete_verified_windows_executables() {
        let mut bytes = [0; 96];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..68].copy_from_slice(b"PE\0\0");
        let mut release = Release {
            tag: "v1.0.0".into(),
            notes: String::new(),
            image_url: String::new(),
            image_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let cancel = AtomicBool::new(false);
        let saved = stage(&bytes[..], &release, &cancel, |_| {}).unwrap();
        assert_eq!(std::fs::read(saved.path()).unwrap(), bytes);
        assert!(stage(&bytes[..95], &release, &cancel, |_| {}).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(
            stage(&bytes[..], &release, &cancel, |_| {})
                .unwrap_err()
                .contains("cancelled")
        );
        cancel.store(false, Ordering::Relaxed);
        bytes[0] = 0;
        assert!(
            stage(&bytes[..], &release, &cancel, |_| {})
                .unwrap_err()
                .contains("SHA-256")
        );
        release.sha256 = format!("{:x}", Sha256::digest(bytes));
        assert!(
            stage(&bytes[..], &release, &cancel, |_| {})
                .unwrap_err()
                .contains("header")
        );
    }
}
