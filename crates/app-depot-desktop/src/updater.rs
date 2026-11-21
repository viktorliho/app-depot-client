//! Official AppImage updates. No App Depot Server profile or library state is used.
use eframe::egui;
use fs2::available_space;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use url::Url;

const REPO: &str = "viktorliho/app-depot-client";
const API: &str = "https://api.github.com/repos/viktorliho/app-depot-client/releases/latest";
const MAX_METADATA: u64 = 1_048_576;
const MAX_IMAGE: u64 = 600 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Version(u32, u32, u32);

impl Version {
    fn parse(value: &str) -> Option<Self> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let mut fields = value.split('.');
        fn field(value: Option<&str>) -> Option<u32> {
            let value = value?;
            if value.is_empty()
                || (value.len() > 1 && value.starts_with('0'))
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            value.parse().ok()
        }
        let version = Self(
            field(fields.next())?,
            field(fields.next())?,
            field(fields.next())?,
        );
        fields.next().is_none().then_some(version)
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    pub image_url: String,
    pub image_size: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    assets: Vec<ApiAsset>,
}
#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

fn expected_url(tag: &str, filename: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{filename}")
}

fn parse_release(bytes: &[u8], current: Version) -> Result<Option<(Release, String)>, String> {
    let value: ApiRelease =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid release metadata: {e}"))?;
    if value.draft || value.prerelease {
        return Ok(None);
    }
    let version = Version::parse(&value.tag_name).ok_or("Malformed release version")?;
    if value.tag_name != format!("v{}.{}.{}", version.0, version.1, version.2) || version <= current
    {
        return Ok(None);
    }
    let filename = format!(
        "AppDepot-{}.{}.{}-x86_64.AppImage",
        version.0, version.1, version.2
    );
    let checksum_name = format!("{filename}.sha256");
    let unique = |name: &str| -> Result<&ApiAsset, String> {
        let mut matches = value.assets.iter().filter(|asset| asset.name == name);
        let first = matches
            .next()
            .ok_or_else(|| format!("Release is missing {name}"))?;
        if matches.next().is_some() {
            return Err(format!("Release has duplicate {name} assets"));
        }
        if first.browser_download_url != expected_url(&value.tag_name, name) {
            return Err(format!("Release asset URL is unexpected: {name}"));
        }
        Ok(first)
    };
    let image = unique(&filename)?;
    let checksum = unique(&checksum_name)?;
    if image.size == 0 || image.size > MAX_IMAGE || checksum.size == 0 || checksum.size > 4096 {
        return Err("Release asset size is missing or unsupported".into());
    }
    let sha256 = image
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or("GitHub did not provide the image SHA-256 digest")?;
    if !hex_digest(sha256) {
        return Err("Invalid image SHA-256 digest".into());
    }
    Ok(Some((
        Release {
            tag: value.tag_name,
            notes: value
                .body
                .unwrap_or_default()
                .chars()
                .take(20_000)
                .collect(),
            image_url: image.browser_download_url.clone(),
            image_size: image.size,
            sha256: sha256.to_owned(),
        },
        checksum.browser_download_url.clone(),
    )))
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn checksum_digest(bytes: &[u8], tag: &str) -> Result<String, String> {
    let version = tag.strip_prefix('v').ok_or("Invalid release tag")?;
    let name = format!("AppDepot-{version}-x86_64.AppImage");
    let line = std::str::from_utf8(bytes).map_err(|_| "Checksum is not UTF-8")?;
    let mut fields = line.trim_end().split("  ");
    let digest = fields.next().ok_or("Missing checksum digest")?;
    let asset = fields.next().ok_or("Missing checksum filename")?;
    if fields.next().is_some() || !hex_digest(digest) || asset != name {
        return Err("Invalid checksum file".into());
    }
    Ok(digest.to_ascii_lowercase())
}

fn allowed_redirect(source: &Url, target: &Url) -> bool {
    if source.scheme() != "https"
        || target.scheme() != "https"
        || target.username() != ""
        || target.password().is_some()
    {
        return false;
    }
    matches!(
        target.host_str(),
        Some("api.github.com" | "github.com" | "release-assets.githubusercontent.com")
    ) && target.port_or_known_default() == Some(443)
}

fn request(url: &str) -> Result<ureq::Response, String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(20 * 60))
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(10))
        .build();
    let mut current = Url::parse(url).map_err(|_| "Invalid update URL")?;
    if current.scheme() != "https"
        || current.username() != ""
        || current.password().is_some()
        || current.port_or_known_default() != Some(443)
        || !matches!(current.host_str(), Some("api.github.com" | "github.com"))
    {
        return Err("Update URL is outside the official GitHub HTTPS hosts".into());
    }
    for _ in 0..=3 {
        let accept = if current.host_str() == Some("api.github.com") {
            "application/vnd.github+json"
        } else {
            "application/octet-stream"
        };
        let response = agent
            .get(current.as_str())
            .set(
                "User-Agent",
                concat!("AppDepotDesktop/", env!("CARGO_PKG_VERSION")),
            )
            .set("Accept", accept)
            .call();
        match response {
            Ok(result) => return Ok(result),
            Err(ureq::Error::Status(code, result)) if (300..400).contains(&code) => {
                let location = result
                    .header("Location")
                    .ok_or("Update redirect has no Location")?;
                let next = current
                    .join(location)
                    .map_err(|_| "Invalid update redirect")?;
                if !allowed_redirect(&current, &next) {
                    return Err("Update redirect left the trusted GitHub HTTPS hosts".into());
                }
                current = next;
            }
            Err(ureq::Error::Status(403 | 429, _)) => {
                return Err("GitHub rate limit or access restriction; try later".into());
            }
            Err(ureq::Error::Status(404, _)) if current.as_str() == API => {
                return Err("No published stable client release yet".into());
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("Update server returned HTTP {code}"));
            }
            Err(error) => return Err(format!("Update network error: {error}")),
        }
    }
    Err("Too many update redirects".into())
}

fn read_bounded(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    request(url)?
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Update metadata is too large".into());
    }
    Ok(bytes)
}

fn check() -> Result<Option<Release>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok_or("Invalid running version")?;
    let bytes = read_bounded(API, MAX_METADATA)?;
    let Some((release, checksum_url)) = parse_release(&bytes, current)? else {
        return Ok(None);
    };
    let checksum = read_bounded(&checksum_url, 4096)?;
    if checksum_digest(&checksum, &release.tag)? != release.sha256 {
        return Err("GitHub release digest and checksum asset disagree".into());
    }
    Ok(Some(release))
}

fn appimage_header(path: &Path) -> io::Result<bool> {
    let mut header = [0_u8; 11];
    File::open(path)?.read_exact(&mut header)?;
    Ok(&header[..4] == b"\x7fELF" && &header[8..11] == b"AI\x02")
}

#[derive(Clone, Debug)]
pub enum InstallMode {
    OfficialAppImage { image: PathBuf, helper: PathBuf },
    Development,
    Unsupported(String),
}

pub fn install_mode() -> InstallMode {
    if env!("APP_DEPOT_BUILD_KIND") != "official" {
        return InstallMode::Development;
    }
    let (Some(image), Some(appdir)) = (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR"))
    else {
        return InstallMode::Unsupported("This build is not running from an AppImage.".into());
    };
    let image = PathBuf::from(image);
    let helper = PathBuf::from(appdir).join("usr/bin/app-depot-update-helper");
    let good = image.is_absolute()
        && fs::symlink_metadata(&image)
            .is_ok_and(|m| m.is_file() && m.uid() == unsafe { libc::geteuid() })
        && image.parent().is_some_and(|parent| {
            fs::symlink_metadata(parent)
                .is_ok_and(|m| m.is_dir() && m.uid() == unsafe { libc::geteuid() })
        })
        && helper.is_file()
        && appimage_header(&image).unwrap_or(false);
    if good {
        InstallMode::OfficialAppImage { image, helper }
    } else {
        InstallMode::Unsupported(
            "The AppImage or its installation folder is not safely user-owned and writable.".into(),
        )
    }
}

#[derive(Clone, Debug)]
pub enum UpdateEvent {
    Checked(Option<Release>),
    Progress(u64, u64),
    Staged {
        path: PathBuf,
        old_sha: String,
        new_sha: String,
    },
    Error(String),
}

pub struct Updater {
    pub mode: InstallMode,
    pub release: Option<Release>,
    pub staged: Option<(PathBuf, String, String)>,
    pub status: String,
    pub progress: Option<(u64, u64)>,
    pub busy: bool,
    pub dismissed: bool,
    pub last_error: Option<String>,
    last_check: Option<Instant>,
    tx: Sender<UpdateEvent>,
    rx: Receiver<UpdateEvent>,
}

impl Updater {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let mode = install_mode();
        let last_error = if let InstallMode::OfficialAppImage { image, .. } = &mode {
            image.parent().and_then(|parent| {
                let path = parent.join(".appdepot-update-error");
                let message = fs::symlink_metadata(&path)
                    .ok()
                    .filter(|meta| {
                        meta.is_file()
                            && meta.uid() == unsafe { libc::geteuid() }
                            && meta.len() <= 4096
                    })
                    .and_then(|_| fs::read_to_string(&path).ok())
                    .map(|s| s.chars().take(1024).collect());
                if message.is_some() {
                    let _ = fs::remove_file(path);
                }
                message
            })
        } else {
            None
        };
        let mut this = Self {
            mode,
            release: None,
            staged: None,
            status: "Not checked".into(),
            progress: None,
            busy: false,
            dismissed: false,
            last_error,
            last_check: None,
            tx,
            rx,
        };
        if matches!(this.mode, InstallMode::OfficialAppImage { .. }) {
            this.check(ctx);
        }
        this
    }
    pub fn check(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "Checking for updates…".into();
        self.last_check = Some(Instant::now());
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let event = match check() {
                Ok(release) => UpdateEvent::Checked(release),
                Err(error) => UpdateEvent::Error(error),
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
    pub fn periodic(&mut self, ctx: &egui::Context) {
        if matches!(self.mode, InstallMode::OfficialAppImage { .. })
            && self
                .last_check
                .is_some_and(|at| at.elapsed() >= CHECK_INTERVAL)
            && !self.busy
        {
            self.check(ctx);
        }
    }
    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                UpdateEvent::Checked(release) => {
                    if release.as_ref().map(|r| &r.tag) != self.release.as_ref().map(|r| &r.tag) {
                        self.dismissed = false;
                    }
                    self.status = release.as_ref().map_or("Up to date".into(), |r| {
                        format!("Update available — {}", r.tag)
                    });
                    self.release = release;
                    self.busy = false;
                }
                UpdateEvent::Progress(done, total) => {
                    self.progress = Some((done, total));
                    self.status = "Downloading update…".into();
                }
                UpdateEvent::Staged {
                    path,
                    old_sha,
                    new_sha,
                } => {
                    self.staged = Some((path, old_sha, new_sha));
                    self.progress = None;
                    self.busy = false;
                    self.status = "Ready to restart".into();
                }
                UpdateEvent::Error(error) => {
                    self.progress = None;
                    self.busy = false;
                    self.status = error;
                }
            }
        }
    }
    pub fn download(&mut self, ctx: &egui::Context) {
        let (InstallMode::OfficialAppImage { image, .. }, Some(release)) =
            (&self.mode, &self.release)
        else {
            return;
        };
        if self.busy || self.staged.is_some() {
            return;
        }
        self.busy = true;
        self.status = "Downloading update…".into();
        let (image, release, tx, ctx) =
            (image.clone(), release.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let event = match stage(&image, &release, &tx, &ctx) {
                Ok((path, old_sha)) => UpdateEvent::Staged {
                    path,
                    old_sha,
                    new_sha: release.sha256.clone(),
                },
                Err(error) => UpdateEvent::Error(error),
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
    pub fn handoff(&mut self) -> Result<(), String> {
        let InstallMode::OfficialAppImage { image, helper } = &self.mode else {
            return Err("In-place updates are unavailable for this build".into());
        };
        let (staged, old_sha, new_sha) =
            self.staged.as_ref().ok_or("Update has not been staged")?;
        let parent = image.parent().ok_or("Invalid AppImage path")?;
        let helper_copy = parent.join(format!(".appdepot-update-helper-{}", uuid::Uuid::new_v4()));
        let backup = parent.join(format!(
            ".AppDepot-{}-previous.AppImage",
            env!("CARGO_PKG_VERSION")
        ));
        let old_size = fs::metadata(image)
            .map_err(|e| format!("Cannot inspect installed AppImage: {e}"))?
            .len();
        if available_space(parent).map_err(|e| e.to_string())?
            < old_size.saturating_add(16 * 1024 * 1024)
        {
            return Err("Not enough free space to preserve the previous AppImage".into());
        }
        if fs::symlink_metadata(&backup).is_ok() {
            return Err(
                "Previous-version backup already exists; inspect it before updating".into(),
            );
        }
        let mut helper_created = false;
        let stage_helper = (|| {
            let mut helper_source = File::open(helper)
                .map_err(|e| format!("Cannot read bundled update helper: {e}"))?;
            let mut helper_destination = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&helper_copy)
                .map_err(|e| format!("Cannot create update helper: {e}"))?;
            helper_created = true;
            let copied = io::copy(&mut helper_source, &mut helper_destination)
                .map_err(|e| format!("Cannot stage update helper: {e}"))?;
            if copied == 0 {
                return Err("Update helper is empty".into());
            }
            helper_destination.sync_all().map_err(|e| e.to_string())?;
            fs::set_permissions(&helper_copy, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            File::open(&helper_copy)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })();
        if let Err(error) = stage_helper {
            if helper_created {
                let _ = fs::remove_file(&helper_copy);
            }
            return Err(error);
        }
        let spawn = std::process::Command::new(&helper_copy)
            .arg("--apply")
            .arg(image)
            .arg(staged)
            .arg(backup)
            .arg(std::process::id().to_string())
            .arg(old_sha)
            .arg(new_sha)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(error) = spawn {
            let _ = fs::remove_file(helper_copy);
            return Err(format!("Cannot start update helper: {error}"));
        }
        Ok(())
    }
}

fn digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn stream_verified<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    expected_size: u64,
    expected_sha256: &str,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes = bytes.checked_add(n as u64).ok_or("Update size overflow")?;
        if bytes > expected_size {
            return Err("Update response exceeded expected length".into());
        }
        writer.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        hash.update(&buffer[..n]);
        progress(bytes);
    }
    if bytes != expected_size {
        return Err("Update download was truncated".into());
    }
    if format!("{:x}", hash.finalize()) != expected_sha256 {
        return Err("Update SHA-256 mismatch".into());
    }
    Ok(())
}

fn stage(
    image: &Path,
    release: &Release,
    tx: &Sender<UpdateEvent>,
    ctx: &egui::Context,
) -> Result<(PathBuf, String), String> {
    let parent = image.parent().ok_or("Invalid AppImage path")?;
    if available_space(parent).map_err(|e| e.to_string())?
        < release.image_size.saturating_add(16 * 1024 * 1024)
    {
        return Err("Not enough free space beside the installed AppImage".into());
    }
    let old_sha = digest(image)?;
    let staged = parent.join(format!(
        ".appdepot-update-{}.AppImage",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|e| format!("Cannot stage update beside AppImage: {e}"))?;
        let response = request(&release.image_url)?;
        if let Some(length) = response.header("Content-Length")
            && length.parse::<u64>().ok() != Some(release.image_size)
        {
            return Err("Update response length differs from GitHub release metadata".into());
        }
        let mut reader = response.into_reader();
        let mut last_report = 0;
        stream_verified(
            &mut reader,
            &mut file,
            release.image_size,
            &release.sha256,
            |bytes| {
                if bytes.saturating_sub(last_report) >= 1024 * 1024 || bytes == release.image_size {
                    last_report = bytes;
                    let _ = tx.send(UpdateEvent::Progress(bytes, release.image_size));
                    ctx.request_repaint();
                }
            },
        )?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        if !appimage_header(&staged).map_err(|e| e.to_string())? {
            return Err("Downloaded file is not a Type 2 AppImage".into());
        }
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok((staged.clone(), old_sha))
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_are_numeric_and_strict() {
        assert!(Version::parse("v0.10.0") > Version::parse("v0.9.9"));
        for value in ["v0.1", "v0.1.0-beta", "v01.0.0", "v0.1.0.1", "v0.x.0"] {
            assert!(Version::parse(value).is_none());
        }
    }
    #[test]
    fn release_requires_exact_asset_urls_digest_and_checksum() {
        let bytes = br#"{"tag_name":"v0.2.0","draft":false,"prerelease":false,"body":"notes","assets":[{"name":"AppDepot-0.2.0-x86_64.AppImage","size":42,"browser_download_url":"https://github.com/viktorliho/app-depot-client/releases/download/v0.2.0/AppDepot-0.2.0-x86_64.AppImage","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"name":"AppDepot-0.2.0-x86_64.AppImage.sha256","size":100,"browser_download_url":"https://github.com/viktorliho/app-depot-client/releases/download/v0.2.0/AppDepot-0.2.0-x86_64.AppImage.sha256","digest":null}]}"#;
        let (release, _) = parse_release(bytes, Version(0, 1, 0)).unwrap().unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(checksum_digest(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  AppDepot-0.2.0-x86_64.AppImage\n", &release.tag).unwrap(), release.sha256);
        assert!(parse_release(bytes, Version(0, 2, 0)).unwrap().is_none());
        let bad = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("github.com/viktorliho", "evil.example/viktorliho");
        assert!(parse_release(bad.as_bytes(), Version(0, 1, 0)).is_err());
        let mut duplicate: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        let image_asset = duplicate["assets"][0].clone();
        duplicate["assets"]
            .as_array_mut()
            .unwrap()
            .push(image_asset);
        assert!(
            parse_release(&serde_json::to_vec(&duplicate).unwrap(), Version(0, 1, 0))
                .unwrap_err()
                .contains("duplicate")
        );
        let mut no_digest: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        no_digest["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(parse_release(&serde_json::to_vec(&no_digest).unwrap(), Version(0, 1, 0)).is_err());
    }
    #[test]
    fn redirect_hosts_fail_closed() {
        let source = Url::parse(API).unwrap();
        assert!(allowed_redirect(
            &source,
            &Url::parse("https://release-assets.githubusercontent.com/x").unwrap()
        ));
        assert!(!allowed_redirect(
            &source,
            &Url::parse("http://github.com/x").unwrap()
        ));
        assert!(!allowed_redirect(
            &source,
            &Url::parse("https://evil.example/x").unwrap()
        ));
    }
    #[test]
    fn transfer_rejects_truncation_overflow_and_digest_mismatch() {
        let data = b"verified AppImage fixture";
        let expected = format!("{:x}", Sha256::digest(data));
        let mut received = Vec::new();
        let mut progress = Vec::new();
        stream_verified(
            &mut &data[..],
            &mut received,
            data.len() as u64,
            &expected,
            |n| progress.push(n),
        )
        .unwrap();
        assert_eq!(received, data);
        assert_eq!(progress.last(), Some(&(data.len() as u64)));
        assert!(
            stream_verified(
                &mut &data[..data.len() - 1],
                &mut Vec::new(),
                data.len() as u64,
                &expected,
                |_| {}
            )
            .unwrap_err()
            .contains("truncated")
        );
        assert!(
            stream_verified(
                &mut &data[..],
                &mut Vec::new(),
                (data.len() - 1) as u64,
                &expected,
                |_| {}
            )
            .unwrap_err()
            .contains("exceeded")
        );
        assert!(
            stream_verified(
                &mut &data[..],
                &mut Vec::new(),
                data.len() as u64,
                &"0".repeat(64),
                |_| {}
            )
            .unwrap_err()
            .contains("SHA-256")
        );
    }
    #[test]
    fn malformed_release_metadata_fails_closed() {
        let base = r#"{"tag_name":"v0.2.0","draft":false,"prerelease":false,"assets":[]}"#;
        assert!(parse_release(base.as_bytes(), Version(0, 1, 0)).is_err());
        assert!(
            parse_release(
                base.replace("v0.2.0", "v0.2.0-rc.1").as_bytes(),
                Version(0, 1, 0)
            )
            .is_err()
        );
        assert!(
            parse_release(
                base.replace("\"prerelease\":false", "\"prerelease\":true")
                    .as_bytes(),
                Version(0, 1, 0)
            )
            .unwrap()
            .is_none()
        );
        assert!(checksum_digest(b"bad checksum", "v0.2.0").is_err());
        assert!(checksum_digest(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  Other.AppImage\n", "v0.2.0").is_err());
    }
}
