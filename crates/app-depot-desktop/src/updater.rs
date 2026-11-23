//! Official Desktop release discovery, independent of App Depot Server profiles.
use eframe::egui;
#[cfg(target_os = "linux")]
use fs2::available_space;
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use std::{
    io::{Read, Write},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use url::Url;

const REPO: &str = "viktorliho/app-depot-client";
const API: &str = "https://api.github.com/repos/viktorliho/app-depot-client/releases/latest";
const RELEASES: &str =
    "https://api.github.com/repos/viktorliho/app-depot-client/releases?per_page=100";
const MAX_METADATA: u64 = 1_048_576;
const MAX_IMAGE: u64 = 600 * 1024 * 1024;
const SETUP: &str = "AppDepot-Desktop-Windows-x64-Setup.exe";
const MANIFEST: &str = "SHA256SUMS";
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

fn asset<'a>(
    release: &'a ApiRelease,
    name: &str,
    max: u64,
) -> Result<(&'a ApiAsset, String), String> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .ok_or_else(|| format!("Release is missing {name}"))?;
    if matches.next().is_some()
        || asset.size == 0
        || asset.size > max
        || asset.browser_download_url != expected_url(&release.tag_name, name)
    {
        return Err(format!("Invalid release asset: {name}"));
    }
    let hash = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| hex_digest(digest))
        .ok_or_else(|| format!("GitHub SHA-256 is missing or invalid for {name}"))?;
    Ok((asset, hash.to_ascii_lowercase()))
}

#[cfg(windows)]
#[path = "windows_release.rs"]
mod windows_release;

#[cfg(all(test, target_os = "linux"))]
fn parse_release(bytes: &[u8], current: Version) -> Result<Option<(Release, String)>, String> {
    let value: ApiRelease =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid release metadata: {e}"))?;
    parse_linux_release(value, current)
}

#[cfg(target_os = "linux")]
fn parse_linux_release(
    value: ApiRelease,
    current: Version,
) -> Result<Option<(Release, String)>, String> {
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

#[cfg(target_os = "linux")]
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
    let current = Url::parse(url).map_err(|_| "Invalid update URL")?;
    if current.scheme() != "https"
        || current.username() != ""
        || current.password().is_some()
        || current.port_or_known_default() != Some(443)
        || !matches!(current.host_str(), Some("api.github.com" | "github.com"))
    {
        return Err("Update URL is outside the official GitHub HTTPS hosts".into());
    }
    follow_redirects(&agent, current, allowed_redirect)
}

fn follow_redirects(
    agent: &ureq::Agent,
    mut current: Url,
    allowed: impl Fn(&Url, &Url) -> bool,
) -> Result<ureq::Response, String> {
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
            // ureq 2.x returns 3xx as Ok when automatic redirects are disabled.
            Ok(result) if (300..400).contains(&result.status()) => {
                let location = result
                    .header("Location")
                    .ok_or("Update redirect has no Location")?;
                let next = current
                    .join(location)
                    .map_err(|_| "Invalid update redirect")?;
                if !allowed(&current, &next) {
                    return Err("Update redirect left the trusted GitHub HTTPS hosts".into());
                }
                current = next;
            }
            Ok(result) if result.status() == 200 => return Ok(result),
            Ok(result) => return Err(format!("Update server returned HTTP {}", result.status())),
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

#[derive(Clone, Copy)]
enum ResponseKind {
    ReleaseJson,
    Checksum,
    #[cfg(target_os = "linux")]
    AppImage,
    #[cfg(windows)]
    WindowsInstaller,
}

fn validate_response_kind(response: &ureq::Response, kind: ResponseKind) -> Result<(), String> {
    let content_type = response.header("Content-Type").unwrap_or("");
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let valid_type = match kind {
        ResponseKind::ReleaseJson => matches!(
            media_type.as_str(),
            "application/json" | "application/vnd.github+json"
        ),
        ResponseKind::Checksum => matches!(
            media_type.as_str(),
            "application/octet-stream" | "text/plain"
        ),
        #[cfg(target_os = "linux")]
        ResponseKind::AppImage => media_type == "application/octet-stream",
        #[cfg(windows)]
        ResponseKind::WindowsInstaller => matches!(
            media_type.as_str(),
            "application/octet-stream"
                | "application/x-msdownload"
                | "application/vnd.microsoft.portable-executable"
        ),
    };
    if !valid_type {
        return Err(format!(
            "Unexpected update response Content-Type: {content_type}; expected {}",
            match kind {
                ResponseKind::ReleaseJson => "GitHub release JSON",
                ResponseKind::Checksum => "a checksum asset",
                #[cfg(target_os = "linux")]
                ResponseKind::AppImage => "an AppImage asset",
                #[cfg(windows)]
                ResponseKind::WindowsInstaller => "a Windows installer asset",
            }
        ));
    }
    Ok(())
}

fn read_response_bounded(
    response: ureq::Response,
    limit: u64,
    kind: ResponseKind,
) -> Result<Vec<u8>, String> {
    validate_response_kind(&response, kind)?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Update metadata is too large".into());
    }
    Ok(bytes)
}

fn read_bounded(url: &str, limit: u64, kind: ResponseKind) -> Result<Vec<u8>, String> {
    read_response_bounded(request(url)?, limit, kind)
}

fn release_history() -> Result<Vec<ApiRelease>, String> {
    let mut releases = Vec::new();
    for page in 1..=3 {
        let bytes = read_bounded(
            &format!("{RELEASES}&page={page}"),
            8 * MAX_METADATA,
            ResponseKind::ReleaseJson,
        )?;
        let batch: Vec<ApiRelease> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid GitHub releases: {error}"))?;
        let last = batch.len() < 100;
        releases.extend(batch);
        if last {
            return Ok(releases);
        }
    }
    Err("GitHub release history exceeded the Desktop discovery limit".into())
}

fn complete_desktop_version(release: &ApiRelease) -> Option<Version> {
    if release.draft || release.prerelease {
        return None;
    }
    let tag = release.tag_name.strip_prefix('v')?;
    let version = Version::parse(tag)?;
    if tag != format!("{}.{}.{}", version.0, version.1, version.2) {
        return None;
    }
    let image = format!("AppDepot-{tag}-x86_64.AppImage");
    for (name, limit) in [
        (image.as_str(), MAX_IMAGE),
        (&format!("{image}.sha256"), 4096),
        (&format!("{image}.zsync"), MAX_IMAGE),
        (SETUP, MAX_IMAGE),
        (MANIFEST, 4096),
    ] {
        asset(release, name, limit).ok()?;
    }
    Some(version)
}

fn latest_desktop_release(
    latest: ApiRelease,
    current: Version,
    history: impl FnOnce() -> Result<Vec<ApiRelease>, String>,
) -> Result<Option<ApiRelease>, String> {
    if let Some(version) = complete_desktop_version(&latest) {
        return Ok((version > current).then_some(latest));
    }
    Ok(history()?
        .into_iter()
        .filter_map(|release| {
            let version = complete_desktop_version(&release)?;
            (version > current).then_some((version, release))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, release)| release))
}

fn discover_desktop_release(current: Version) -> Result<Option<ApiRelease>, String> {
    let bytes = read_bounded(API, MAX_METADATA, ResponseKind::ReleaseJson)?;
    let latest: ApiRelease =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid release metadata: {e}"))?;
    latest_desktop_release(latest, current, release_history)
}

#[cfg(target_os = "linux")]
fn check() -> Result<Option<Release>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok_or("Invalid running version")?;
    let Some(latest) = discover_desktop_release(current)? else {
        return Ok(None);
    };
    let Some((release, checksum_url)) = parse_linux_release(latest, current)? else {
        return Ok(None);
    };
    let checksum = read_bounded(&checksum_url, 4096, ResponseKind::Checksum)?;
    if checksum_digest(&checksum, &release.tag)? != release.sha256 {
        return Err("GitHub release digest and checksum asset disagree".into());
    }
    Ok(Some(release))
}

#[cfg(windows)]
use windows_release::check;

#[cfg(target_os = "linux")]
fn appimage_header(path: &Path) -> io::Result<bool> {
    let mut header = [0_u8; 11];
    File::open(path)?.read_exact(&mut header)?;
    Ok(&header[..4] == b"\x7fELF" && &header[8..11] == b"AI\x02")
}

#[derive(Clone, Debug)]
pub enum InstallMode {
    #[cfg(target_os = "linux")]
    OfficialAppImage { image: PathBuf, helper: PathBuf },
    #[cfg(target_os = "linux")]
    Development,
    #[cfg(windows)]
    WindowsStable,
    #[cfg(target_os = "linux")]
    Unsupported(String),
}

#[cfg(target_os = "linux")]
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

#[derive(Debug)]
pub enum UpdateEvent {
    Checked(Option<Release>),
    Progress(u64, u64),
    #[cfg(windows)]
    WindowsStaged(windows_release::VerifiedInstaller),
    #[cfg(windows)]
    Cancelled,
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    pub staged: Option<(PathBuf, String, String)>,
    #[cfg(windows)]
    pub staged: Option<windows_release::VerifiedInstaller>,
    pub status: String,
    pub progress: Option<(u64, u64)>,
    pub busy: bool,
    pub dismissed: bool,
    pub last_error: Option<String>,
    last_check: Option<Instant>,
    #[cfg(windows)]
    pub download_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(windows)]
    pub confirm_windows_update: bool,
    tx: Sender<UpdateEvent>,
    rx: Receiver<UpdateEvent>,
}

impl Updater {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        #[cfg(windows)]
        let mode = InstallMode::WindowsStable;
        #[cfg(target_os = "linux")]
        let mode = install_mode();
        #[cfg(target_os = "linux")]
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
            #[cfg(target_os = "linux")]
            last_error,
            #[cfg(windows)]
            last_error: None,
            last_check: None,
            #[cfg(windows)]
            download_cancel: None,
            #[cfg(windows)]
            confirm_windows_update: false,
            tx,
            rx,
        };
        if this.check_automatically() {
            this.check(ctx);
        }
        this
    }
    fn check_automatically(&self) -> bool {
        match self.mode {
            #[cfg(target_os = "linux")]
            InstallMode::OfficialAppImage { .. } => true,
            #[cfg(target_os = "linux")]
            _ => false,
            #[cfg(windows)]
            InstallMode::WindowsStable => true,
        }
    }
    pub fn can_check(&self) -> bool {
        #[cfg(windows)]
        if self.staged.is_some() {
            return false;
        }
        !self.busy
    }
    pub fn check(&mut self, ctx: &egui::Context) {
        if !self.can_check() {
            return;
        }
        #[cfg(windows)]
        {
            self.release = None;
        }
        self.busy = true;
        self.status = "Checking for updatesÃ¢â‚¬Â¦".into();
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
        if self.check_automatically()
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
                    #[cfg(target_os = "linux")]
                    let no_update = "Up to date";
                    #[cfg(windows)]
                    let no_update = "No newer complete stable Desktop release found";
                    self.status = release.as_ref().map_or(no_update.into(), |r| {
                        format!("Update available ({})", r.tag)
                    });
                    self.release = release;
                    self.busy = false;
                }
                UpdateEvent::Progress(done, total) => {
                    self.progress = Some((done, total));
                    self.status = "Downloading updateÃ¢â‚¬Â¦".into();
                }
                #[cfg(target_os = "linux")]
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
                #[cfg(windows)]
                UpdateEvent::WindowsStaged(installer) => {
                    let cancelled = self
                        .download_cancel
                        .take()
                        .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed));
                    if !cancelled {
                        self.staged = Some(installer);
                    }
                    self.progress = None;
                    self.busy = false;
                    self.status = if cancelled {
                        "Update download cancelled"
                    } else {
                        "Installer downloaded and verified"
                    }
                    .into();
                }
                #[cfg(windows)]
                UpdateEvent::Cancelled => {
                    self.download_cancel = None;
                    self.progress = None;
                    self.busy = false;
                    self.status = "Update download cancelled".into();
                }
                UpdateEvent::Error(error) => {
                    self.progress = None;
                    #[cfg(windows)]
                    {
                        self.download_cancel = None;
                    }
                    self.busy = false;
                    self.status = error;
                }
            }
        }
    }
    #[cfg(windows)]
    pub fn download(&mut self, ctx: &egui::Context) {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let Some(release) = self.release.clone() else {
            return;
        };
        if self.busy || self.staged.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.download_cancel = Some(Arc::clone(&cancel));
        self.busy = true;
        self.status = "Downloading updateÃ¢â‚¬Â¦".into();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let result = windows_release::download(&release, &cancel, |bytes| {
                let _ = tx.send(UpdateEvent::Progress(bytes, release.image_size));
                ctx.request_repaint();
            });
            let event = if cancel.load(Ordering::Relaxed) {
                UpdateEvent::Cancelled
            } else {
                match result {
                    Ok(installer) => UpdateEvent::WindowsStaged(installer),
                    Err(error) => UpdateEvent::Error(error),
                }
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
    #[cfg(target_os = "linux")]
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
        self.status = "Downloading updateÃ¢â‚¬Â¦".into();
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
    #[cfg(target_os = "linux")]
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

    #[cfg(windows)]
    pub fn handoff(&mut self) -> Result<(), String> {
        let stage = self.staged.as_ref().ok_or("Update has not been staged")?;
        stage.start_update()?;
        self.staged.take().unwrap().keep_for_setup();
        Ok(())
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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
        validate_response_kind(&response, ResponseKind::AppImage)?;
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
mod channel_tests {
    use super::*;

    fn complete(tag: &str) -> ApiRelease {
        let image = format!("AppDepot-{}-x86_64.AppImage", tag.trim_start_matches('v'));
        let names = [
            image.clone(),
            format!("{image}.sha256"),
            format!("{image}.zsync"),
            SETUP.into(),
            MANIFEST.into(),
        ];
        ApiRelease {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            body: None,
            assets: names
                .into_iter()
                .map(|name| ApiAsset {
                    size: 100,
                    browser_download_url: expected_url(tag, &name),
                    digest: Some(format!("sha256:{}", "a".repeat(64))),
                    name,
                })
                .collect(),
        }
    }

    #[test]
    fn unified_stable_channels_require_both_platforms_and_a_newer_version() {
        let mut linux_only = complete("v0.1.5");
        linux_only.assets.retain(|asset| asset.name != SETUP);
        let mut windows_only = complete("v0.1.6");
        windows_only
            .assets
            .retain(|asset| [SETUP, MANIFEST].contains(&asset.name.as_str()));
        let mut draft = complete("v0.1.7");
        draft.draft = true;
        let mut prerelease = complete("v0.1.8");
        prerelease.prerelease = true;
        let mut invalid = complete("v0.1.9");
        invalid.assets[0].browser_download_url = "https://github.com/another/repository".into();
        let selected = latest_desktop_release(complete("decky-v8.0.0"), Version(0, 1, 3), || {
            Ok(vec![
                linux_only,
                windows_only,
                draft,
                prerelease,
                invalid,
                complete("windows-v9.0.0"),
                complete("decky-v8.0.0"),
                complete("v0.1.4"),
            ])
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected.tag_name, "v0.1.4");
        assert!(
            asset(&selected, SETUP, MAX_IMAGE)
                .unwrap()
                .0
                .browser_download_url
                .ends_with(".exe")
        );
        assert!(
            asset(&selected, "AppDepot-0.1.4-x86_64.AppImage", MAX_IMAGE)
                .unwrap()
                .0
                .browser_download_url
                .ends_with(".AppImage")
        );
        assert!(
            latest_desktop_release(complete("v0.1.4"), Version(0, 1, 4), || panic!(
                "complete Latest needs no fallback"
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            latest_desktop_release(complete("v0.1.4"), Version(0, 1, 3), || panic!(
                "complete Latest needs no fallback"
            ))
            .unwrap()
            .is_some()
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn github_style_asset_redirect_delivers_checksum_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let checksum = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  AppDepot-0.2.0-x86_64.AppImage\n";
        let server = thread::spawn(move || {
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let n = stream.read(&mut request).unwrap();
                let path = if step < 2 { "/asset" } else { "/cdn" };
                assert!(
                    String::from_utf8_lossy(&request[..n]).starts_with(&format!("GET {path} "))
                );
                if step > 0 {
                    let headers = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
                    assert!(headers.contains("accept: application/octet-stream"));
                    assert!(headers.contains(&format!(
                        "user-agent: appdepotdesktop/{}",
                        env!("CARGO_PKG_VERSION")
                    )));
                }
                if step < 2 {
                    write!(stream, "HTTP/1.1 302 Found\r\nContent-Type: text/html; charset=utf-8\r\nLocation: http://{address}/cdn\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", checksum.len()).unwrap();
                    stream.write_all(checksum).unwrap();
                }
            }
        });
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        // This is the v0.1.0 failure: ureq returns 302 as Ok, with an empty
        // redirect body that the old code passed to checksum_digest.
        let old_response = agent
            .get(&format!("http://{address}/asset"))
            .call()
            .unwrap();
        assert_eq!(old_response.status(), 302);
        let mut old_body = Vec::new();
        old_response
            .into_reader()
            .read_to_end(&mut old_body)
            .unwrap();
        assert_eq!(
            checksum_digest(&old_body, "v0.2.0").unwrap_err(),
            "Missing checksum filename"
        );
        let response = follow_redirects(
            &agent,
            Url::parse(&format!("http://{address}/asset")).unwrap(),
            |_, target| {
                target.host_str() == Some("127.0.0.1") && target.port() == Some(address.port())
            },
        )
        .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = read_response_bounded(response, 4096, ResponseKind::Checksum).unwrap();
        assert_eq!(checksum_digest(&bytes, "v0.2.0").unwrap(), "a".repeat(64));
        server.join().unwrap();
    }

    #[test]
    fn html_checksum_response_is_rejected_before_parsing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let received = stream.read(&mut request).unwrap();
            assert!(received > 0);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 17\r\nConnection: close\r\n\r\n<html>oops</html>").unwrap();
        });
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        let response = follow_redirects(
            &agent,
            Url::parse(&format!("http://{address}/asset")).unwrap(),
            |_, _| false,
        )
        .unwrap();
        assert!(
            read_response_bounded(response, 4096, ResponseKind::Checksum)
                .unwrap_err()
                .contains("Content-Type: text/html")
        );
        server.join().unwrap();
    }

    #[test]
    #[ignore = "requires the public GitHub release service"]
    fn current_official_release_metadata_and_checksum() {
        let metadata = read_bounded(API, MAX_METADATA, ResponseKind::ReleaseJson).unwrap();
        let (release, checksum_url) = parse_release(&metadata, Version(0, 1, 0))
            .unwrap()
            .expect("a stable release newer than v0.1.0");
        let checksum = read_bounded(&checksum_url, 4096, ResponseKind::Checksum).unwrap();
        assert_eq!(
            checksum_digest(&checksum, &release.tag).unwrap(),
            release.sha256
        );
    }
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
