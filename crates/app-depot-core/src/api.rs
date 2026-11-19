//! Read-only App Depot HTTP client. Every route is from the Server OpenAPI.

use crate::models::{
    App, Artifact, Artwork, Association, AssociationArtifact, Capabilities, DeclarationSnapshot,
    InventoryEntry, Page, Release,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt, io::Read, time::Duration};
use url::{Host, Url};

const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: u64 = 64 * 1024;
const MAX_PAGES: usize = 10_000;
const MAX_ITEMS: usize = 100_000;

#[derive(Clone, Debug)]
pub struct Problem {
    pub code: String,
    pub title: String,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum ApiError {
    Config(String),
    Transport(String),
    Authentication(Option<Problem>),
    Authorization(Option<Problem>),
    Incompatible(String),
    Protocol(String),
    Http {
        status: u16,
        problem: Option<Problem>,
    },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "configuration: {message}"),
            Self::Transport(message) => write!(f, "transport: {message}"),
            Self::Authentication(problem) => {
                write!(f, "authentication failed: {}", problem_label(problem))
            }
            Self::Authorization(problem) => {
                write!(f, "authorization failed: {}", problem_label(problem))
            }
            Self::Incompatible(message) => write!(f, "incompatible server: {message}"),
            Self::Protocol(message) => write!(f, "protocol: {message}"),
            Self::Http { status, problem } => {
                write!(f, "HTTP {status}: {}", problem_label(problem))
            }
        }
    }
}

impl std::error::Error for ApiError {}

fn problem_label(problem: &Option<Problem>) -> String {
    match problem {
        Some(problem) => format!("{} ({})", problem.title, problem.code),
        None => "no valid Problem Details body".into(),
    }
}

pub struct ApiClient {
    base: Url,
    agent: ureq::Agent,
    token: Option<String>,
}

impl ApiClient {
    pub fn new(
        server_url: &str,
        token: Option<String>,
        allow_insecure_http: bool,
    ) -> Result<Self, ApiError> {
        let base =
            Url::parse(server_url).map_err(|_| ApiError::Config("invalid server URL".into()))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(ApiError::Config(
                "server URL must be an HTTP(S) origin without credentials, path, query or fragment"
                    .into(),
            ));
        }
        if base.scheme() == "http" && !allow_insecure_http && !is_loopback(&base) {
            return Err(ApiError::Config("remote HTTP requires explicit --allow-insecure-http; Bearer tokens would be exposed in transit".into()));
        }
        if token.as_ref().is_some_and(|token| {
            token.is_empty() || !token.bytes().all(|byte| (33..=126).contains(&byte))
        }) {
            return Err(ApiError::Config("invalid Bearer token".into()));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(20))
            .redirects(0)
            .build();
        Ok(Self { base, agent, token })
    }

    pub fn origin(&self) -> &str {
        self.base.as_str()
    }

    pub(crate) fn file_content(
        &self,
        artifact_id: &str,
        offset: Option<u64>,
        if_range: Option<&str>,
    ) -> Result<ureq::Response, ApiError> {
        if !valid_artifact_id(artifact_id) || !artifact_id.starts_with("adfile:sha256:") {
            return Err(ApiError::Config("invalid File Artifact ID".into()));
        }
        self.binary_get(
            &["artifacts", artifact_id, "content"],
            offset,
            if_range,
            false,
        )
    }

    fn binary_get(
        &self,
        segments: &[&str],
        offset: Option<u64>,
        if_range: Option<&str>,
        close_after_response: bool,
    ) -> Result<ureq::Response, ApiError> {
        let url = self.url(segments, None, None)?;
        let mut request = self
            .agent
            .get(url.as_str())
            .set("Accept", "application/octet-stream");
        if close_after_response {
            request = request.set("Connection", "close");
        }
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        if let Some(offset) = offset {
            request = request.set("Range", &format!("bytes={offset}-"));
            if let Some(etag) = if_range {
                request = request.set("If-Range", etag);
            }
        }
        let response = match request.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(ApiError::Transport(error.to_string()));
            }
        };
        let status = response.status();
        if matches!(status, 200 | 206 | 416) {
            return Ok(response);
        }
        let content_type = response
            .header("Content-Type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        let problem = if content_type == "application/problem+json" {
            read_bounded(response, MAX_ERROR_BYTES)
                .ok()
                .and_then(|body| parse_problem(&body, status))
        } else {
            None
        };
        Err(match status {
            401 => ApiError::Authentication(problem),
            403 => ApiError::Authorization(problem),
            300..=399 => ApiError::Protocol(format!("redirect refused (HTTP {status})")),
            _ => ApiError::Http { status, problem },
        })
    }

    pub(crate) fn tree_file_content(
        &self,
        artifact_id: &str,
        file_id: &str,
        offset: Option<u64>,
        if_range: Option<&str>,
        expected_size: u64,
    ) -> Result<ureq::Response, ApiError> {
        if !valid_artifact_id(artifact_id)
            || !artifact_id.starts_with("adtree:v1:sha256:")
            || uuid::Uuid::parse_str(file_id).is_err()
        {
            return Err(ApiError::Config("invalid Tree Artifact or file ID".into()));
        }
        self.binary_get(
            &["artifacts", artifact_id, "files", file_id],
            offset,
            if_range,
            self.base.scheme() == "http" && expected_size <= 64 * 1024,
        )
    }

    fn url(
        &self,
        segments: &[&str],
        cursor: Option<&str>,
        page_size: Option<u64>,
    ) -> Result<Url, ApiError> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| ApiError::Config("invalid server origin".into()))?;
            path.extend(["api", "v1"]);
            for segment in segments {
                if segment.is_empty()
                    || *segment == "."
                    || *segment == ".."
                    || segment.chars().any(char::is_control)
                {
                    return Err(ApiError::Config("invalid URL path segment".into()));
                }
                path.push(segment);
            }
        }
        if cursor.is_some() || page_size.is_some() {
            let mut query = url.query_pairs_mut();
            if let Some(size) = page_size {
                query.append_pair("pageSize", &size.to_string());
            }
            if let Some(cursor) = cursor {
                query.append_pair("cursor", cursor);
            }
        }
        Ok(url)
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        cursor: Option<&str>,
        page_size: Option<u64>,
    ) -> Result<T, ApiError> {
        self.get_json_with_etag(segments, cursor, page_size)
            .map(|(value, _)| value)
    }

    fn get_json_with_etag<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        cursor: Option<&str>,
        page_size: Option<u64>,
    ) -> Result<(T, Option<String>), ApiError> {
        let url = self.url(segments, cursor, page_size)?;
        let mut request = self
            .agent
            .get(url.as_str())
            .set("Accept", "application/json");
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(ApiError::Transport(error.to_string()));
            }
        };
        let status = response.status();
        let content_type = response
            .header("Content-Type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !(200..300).contains(&status) {
            let problem = if content_type == "application/problem+json" {
                read_bounded(response, MAX_ERROR_BYTES)
                    .ok()
                    .and_then(|body| parse_problem(&body, status))
            } else {
                None
            };
            return Err(match status {
                401 => ApiError::Authentication(problem),
                403 => ApiError::Authorization(problem),
                300..=399 => ApiError::Protocol(format!("redirect refused (HTTP {status})")),
                _ => ApiError::Http { status, problem },
            });
        }
        if content_type != "application/json" {
            return Err(ApiError::Protocol(format!(
                "unexpected Content-Type {content_type:?}"
            )));
        }
        let etag = response.header("ETag").map(str::to_owned);
        let body = read_bounded(response, MAX_JSON_BYTES)?;
        serde_json::from_slice(&body)
            .map(|value| (value, etag))
            .map_err(|error| ApiError::Protocol(format!("malformed JSON response: {error}")))
    }

    pub fn capabilities(&self) -> Result<Capabilities, ApiError> {
        self.get_json(&["capabilities"], None, None)
    }

    pub fn ensure_read_compatible(&self) -> Result<Capabilities, ApiError> {
        let capabilities = self.capabilities()?;
        capabilities
            .read_page_size()
            .map_err(ApiError::Incompatible)?;
        Ok(capabilities)
    }

    pub fn list_apps(&self, page_size: u64) -> Result<Vec<App>, ApiError> {
        self.pages(&["apps"], page_size)
    }

    pub fn app(&self, id: &str) -> Result<App, ApiError> {
        let app: App = self.get_json(&["apps", id], None, None)?;
        if app.id != id {
            return Err(ApiError::Protocol("App ID does not match request".into()));
        }
        Ok(app)
    }

    pub fn list_artwork(&self, app_id: &str) -> Result<Vec<Artwork>, ApiError> {
        #[derive(serde::Deserialize)]
        struct List {
            items: Vec<Artwork>,
        }
        let list: List = self.get_json(&["apps", app_id, "artwork"], None, None)?;
        if list.items.len() > 1000 || list.items.iter().any(|item| item.app_id != app_id) {
            return Err(ApiError::Protocol("invalid artwork list".into()));
        }
        Ok(list.items)
    }

    /// Fetch only the current Server-bound image. The caller decodes it off the UI thread.
    pub fn artwork_bytes(&self, item: &Artwork) -> Result<Vec<u8>, ApiError> {
        const MAX_ARTWORK: u64 = 8 * 1024 * 1024;
        if item.size == 0
            || item.size > MAX_ARTWORK
            || item.width == 0
            || item.height == 0
            || item.width > 8192
            || item.height > 8192
            || u64::from(item.width) * u64::from(item.height) > 16_000_000
            || !matches!(
                item.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || item.digest.len() != 64
            || !item.digest.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ApiError::Protocol("unsafe artwork metadata".into()));
        }
        let url = self.url(
            &["apps", &item.app_id, "artwork", &item.role, "content"],
            None,
            None,
        )?;
        let mut request = self.agent.get(url.as_str()).set("Accept", &item.mime_type);
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let response = match request.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(ApiError::Transport(error.to_string()));
            }
        };
        if response.status() != 200 {
            let status = response.status();
            return Err(match status {
                401 => ApiError::Authentication(None),
                403 => ApiError::Authorization(None),
                300..=399 => ApiError::Protocol(format!("redirect refused (HTTP {status})")),
                _ => ApiError::Http {
                    status,
                    problem: None,
                },
            });
        }
        let mime = response
            .header("Content-Type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if mime != item.mime_type {
            return Err(ApiError::Protocol(
                "artwork MIME type differs from binding".into(),
            ));
        }
        if response
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n != item.size)
        {
            return Err(ApiError::Protocol(
                "artwork length differs from binding".into(),
            ));
        }
        let bytes = read_bounded(response, MAX_ARTWORK)?;
        if bytes.len() as u64 != item.size || format!("{:x}", Sha256::digest(&bytes)) != item.digest
        {
            return Err(ApiError::Protocol("artwork digest or size mismatch".into()));
        }
        Ok(bytes)
    }

    pub fn list_releases(&self, app_id: &str, page_size: u64) -> Result<Vec<Release>, ApiError> {
        let releases: Vec<Release> = self.pages(&["apps", app_id, "releases"], page_size)?;
        if releases.iter().any(|release| release.app_id != app_id) {
            return Err(ApiError::Protocol(
                "Release belongs to a different App".into(),
            ));
        }
        Ok(releases)
    }

    pub fn release(&self, id: &str) -> Result<Release, ApiError> {
        let release: Release = self.get_json(&["releases", id], None, None)?;
        if release.id != id {
            return Err(ApiError::Protocol(
                "Release ID does not match request".into(),
            ));
        }
        Ok(release)
    }

    pub fn list_associations(
        &self,
        release_id: &str,
        page_size: u64,
    ) -> Result<Vec<Association>, ApiError> {
        let associations: Vec<Association> =
            self.pages(&["releases", release_id, "artifacts"], page_size)?;
        if associations
            .iter()
            .any(|association| association.release_id != release_id)
        {
            return Err(ApiError::Protocol(
                "Association belongs to a different Release".into(),
            ));
        }
        Ok(associations)
    }

    pub fn artifact(&self, id: &str) -> Result<Artifact, ApiError> {
        if !valid_artifact_id(id) {
            return Err(ApiError::Config("invalid Artifact ID".into()));
        }
        let artifact: Artifact = self.get_json(&["artifacts", id], None, None)?;
        if artifact.id != id
            || !valid_artifact_id(&artifact.id)
            || !matches!(artifact.kind.as_str(), "File" | "Tree")
            || (artifact.kind == "File") != artifact.id.starts_with("adfile:sha256:")
            || (artifact.kind == "File" && artifact.size.is_none())
        {
            return Err(ApiError::Protocol("invalid Artifact metadata".into()));
        }
        Ok(artifact)
    }

    pub fn tree_inventory(
        &self,
        id: &str,
        page_size: u64,
    ) -> Result<(Vec<InventoryEntry>, String), ApiError> {
        if !valid_artifact_id(id) || !id.starts_with("adtree:v1:sha256:") {
            return Err(ApiError::Config("invalid Tree Artifact ID".into()));
        }
        if !(1..=1000).contains(&page_size) {
            return Err(ApiError::Config("page size outside protocol range".into()));
        }
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        let mut validator: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let (page, etag): (Page<Value>, _) = self.get_json_with_etag(
                &["artifacts", id, "inventory"],
                cursor.as_deref(),
                Some(page_size),
            )?;
            let etag = etag.ok_or_else(|| ApiError::Protocol("inventory ETag missing".into()))?;
            if !is_strong_etag(&etag) || validator.as_ref().is_some_and(|prior| prior != &etag) {
                return Err(ApiError::Protocol(
                    "inventory ETag missing, weak or changed".into(),
                ));
            }
            validator = Some(etag);
            if page.items.is_empty() && page.next_cursor.is_some()
                || page.items.len() > page_size as usize
                || items.len() + page.items.len() > MAX_ITEMS
            {
                return Err(ApiError::Protocol("invalid inventory page size".into()));
            }
            for raw in page.items {
                let entry = parse_inventory_entry(raw)?;
                if items.last().is_some_and(|previous: &InventoryEntry| {
                    previous.path().as_bytes() >= entry.path().as_bytes()
                }) {
                    return Err(ApiError::Protocol(
                        "inventory paths are duplicate or not canonically ordered".into(),
                    ));
                }
                items.push(entry);
            }
            match page.next_cursor {
                None => return Ok((items, validator.unwrap())),
                Some(next) if next.is_empty() || !seen.insert(next.clone()) => {
                    return Err(ApiError::Protocol(
                        "empty or repeated inventory cursor".into(),
                    ));
                }
                Some(next) => cursor = Some(next),
            }
        }
        Err(ApiError::Protocol("inventory exceeds page limit".into()))
    }

    pub fn associated_artifacts(
        &self,
        release_id: &str,
        page_size: u64,
    ) -> Result<Vec<AssociationArtifact>, ApiError> {
        self.list_associations(release_id, page_size)?
            .into_iter()
            .map(|association| {
                if !valid_artifact_id(&association.artifact_id) {
                    return Err(ApiError::Protocol("invalid Association Artifact ID".into()));
                }
                let artifact = self.artifact(&association.artifact_id)?;
                Ok(AssociationArtifact {
                    association,
                    artifact,
                })
            })
            .collect()
    }

    pub fn declarations(&self, release_id: &str) -> Result<DeclarationSnapshot, ApiError> {
        self.declarations_with_etag(release_id)
            .map(|(value, _)| value)
    }

    pub fn declarations_with_etag(
        &self,
        release_id: &str,
    ) -> Result<(DeclarationSnapshot, Option<String>), ApiError> {
        let (snapshot, etag): (DeclarationSnapshot, Option<String>) = self.get_json_with_etag(
            &["releases", release_id, "declaration-snapshot"],
            None,
            None,
        )?;
        if snapshot.release_id != release_id {
            return Err(ApiError::Protocol(
                "snapshot Release ID does not match request".into(),
            ));
        }
        let mut ids = HashSet::new();
        if snapshot
            .associations
            .iter()
            .any(|association| !ids.insert(&association.association_id))
        {
            return Err(ApiError::Protocol(
                "duplicate snapshot Association ID".into(),
            ));
        }
        Ok((snapshot, etag))
    }

    fn pages<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        page_size: u64,
    ) -> Result<Vec<T>, ApiError> {
        if !(1..=1000).contains(&page_size) {
            return Err(ApiError::Config("page size outside protocol range".into()));
        }
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let page: Page<T> = self.get_json(segments, cursor.as_deref(), Some(page_size))?;
            if page.items.len() > page_size as usize || items.len() + page.items.len() > MAX_ITEMS {
                return Err(ApiError::Protocol(
                    "collection exceeds advertised page size or client limit".into(),
                ));
            }
            items.extend(page.items);
            match page.next_cursor {
                None => return Ok(items),
                Some(next) if next.is_empty() || !seen.insert(next.clone()) => {
                    return Err(ApiError::Protocol(
                        "empty or repeated pagination cursor".into(),
                    ));
                }
                Some(next) => cursor = Some(next),
            }
        }
        Err(ApiError::Protocol("collection exceeds page limit".into()))
    }
}

fn valid_artifact_id(id: &str) -> bool {
    let digest = id
        .strip_prefix("adfile:sha256:")
        .or_else(|| id.strip_prefix("adtree:v1:sha256:"));
    digest.is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_strong_etag(value: &str) -> bool {
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value[1..value.len() - 1].contains('"')
        && value.bytes().all(|byte| (33..=126).contains(&byte))
}

fn parse_inventory_entry(raw: Value) -> Result<InventoryEntry, ApiError> {
    let object = raw
        .as_object()
        .ok_or_else(|| ApiError::Protocol("inventory entry is not an object".into()))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Protocol("inventory entry type missing".into()))?;
    let allowed: &[&str] = match kind {
        "File" => &["type", "path", "fileId", "size", "sha256", "executable"],
        "Directory" => &["type", "path", "fileId", "size", "sha256", "executable"],
        _ => {
            return Err(ApiError::Protocol(
                "unsupported inventory entry type".into(),
            ));
        }
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ApiError::Protocol(
            "unexpected inventory entry field".into(),
        ));
    }
    if kind == "Directory"
        && ["fileId", "size", "sha256", "executable"]
            .iter()
            .any(|key| object.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Err(ApiError::Protocol(
            "Directory has file-only metadata".into(),
        ));
    }
    serde_json::from_value(raw)
        .map_err(|error| ApiError::Protocol(format!("malformed inventory entry: {error}")))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    }
}

fn read_bounded(response: ureq::Response, max: u64) -> Result<Vec<u8>, ApiError> {
    if response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max)
    {
        return Err(ApiError::Protocol(
            "JSON response exceeds size limit".into(),
        ));
    }
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|error| ApiError::Transport(error.to_string()))?;
    if body.len() as u64 > max {
        return Err(ApiError::Protocol(
            "JSON response exceeds size limit".into(),
        ));
    }
    Ok(body)
}

fn parse_problem(body: &[u8], http_status: u16) -> Option<Problem> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    let status = object.get("status")?.as_u64()?;
    let code = object.get("code")?.as_str()?;
    let title = object.get("title")?.as_str()?;
    let kind = object.get("type")?.as_str()?;
    if status != u64::from(http_status) || kind != format!("urn:app-depot:problem:{code}") {
        return None;
    }
    Some(Problem {
        code: code.into(),
        title: title.into(),
        detail: object
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AppConvention, AssociationConvention, ReleaseConvention};
    use std::{
        io::Write,
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    const CAPS: &str = r#"{"apiVersion":"1.0.0","ingestionModes":["BareFile","BareTree"],"zipMethods":["Store"],"treeIdentity":"adtree:v1:sha256","fileIdentity":"adfile:sha256","uploadRanges":true,"downloadRanges":true,"limits":{"maxPageSize":1000,"maxConcurrentRanges":8,"maxUploadBytes":18446744073709551615},"features":["serverImport"],"futureNumber":18446744073709551616}"#;

    struct Reply {
        status: u16,
        content_type: &'static str,
        body: String,
        extra_header: Option<String>,
    }

    fn json(body: &str) -> Reply {
        Reply {
            status: 200,
            content_type: "application/json",
            body: body.into(),
            extra_header: None,
        }
    }

    fn server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let handle = thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                received
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                let status_text = match reply.status {
                    200 => "OK",
                    302 => "Found",
                    400 => "Bad Request",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    _ => "Error",
                };
                write!(stream, "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status, status_text, reply.content_type, reply.body.len()).unwrap();
                if let Some(extra) = reply.extra_header {
                    write!(stream, "{extra}\r\n").unwrap();
                }
                let _ = stream.write_all(b"\r\n");
                let _ = stream.write_all(reply.body.as_bytes());
            }
        });
        (address, requests, handle)
    }

    #[test]
    fn capabilities_preserve_unknown_numbers_and_report_incompatibility() {
        let (address, _, handle) = server(vec![json(CAPS)]);
        let caps = ApiClient::new(&address, None, false)
            .unwrap()
            .capabilities()
            .unwrap();
        assert_eq!(caps.limits.max_upload_bytes, u64::MAX);
        assert_eq!(
            caps.unknown["futureNumber"].to_string(),
            "18446744073709551616"
        );
        assert!(caps.incompatibilities().is_empty());
        handle.join().unwrap();

        let incompatible = CAPS
            .replace("adtree:v1:sha256", "future-tree")
            .replace("\"downloadRanges\":true", "\"downloadRanges\":false");
        let caps: Capabilities = serde_json::from_str(&incompatible).unwrap();
        assert_eq!(caps.incompatibilities().len(), 2);
    }

    #[test]
    fn anonymous_and_bearer_requests_are_distinct() {
        let (address, requests, handle) = server(vec![json(CAPS), json(CAPS)]);
        ApiClient::new(&address, None, false)
            .unwrap()
            .capabilities()
            .unwrap();
        ApiClient::new(&address, Some("test-secret".into()), false)
            .unwrap()
            .capabilities()
            .unwrap();
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with("GET /api/v1/capabilities HTTP/1.1"));
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
        assert!(
            requests[1].contains("Authorization: Bearer test-secret")
                || requests[1].contains("authorization: Bearer test-secret")
        );
    }

    #[test]
    fn artwork_uses_profile_auth_and_verifies_binding() {
        let body = "image-bytes";
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
        let item = Artwork {
            app_id: "app-id".into(),
            role: "Icon".into(),
            id: "binding-id".into(),
            digest,
            mime_type: "image/png".into(),
            size: body.len() as u64,
            width: 2,
            height: 2,
            etag: "\"artwork\"".into(),
        };
        let (address, requests, handle) = server(vec![Reply {
            status: 200,
            content_type: "image/png",
            body: body.into(),
            extra_header: None,
        }]);
        let api = ApiClient::new(&address, Some("test-secret".into()), false).unwrap();
        assert_eq!(api.artwork_bytes(&item).unwrap(), body.as_bytes());
        handle.join().unwrap();
        let request = &requests.lock().unwrap()[0];
        assert!(request.starts_with("GET /api/v1/apps/app-id/artwork/Icon/content HTTP/1.1"));
        assert!(request.contains("Bearer test-secret"));
    }

    #[test]
    fn artwork_rejects_bad_mime_digest_and_size() {
        let item = Artwork {
            app_id: "app-id".into(),
            role: "Icon".into(),
            id: "binding-id".into(),
            digest: "0".repeat(64),
            mime_type: "image/png".into(),
            size: 3,
            width: 2,
            height: 2,
            etag: "\"artwork\"".into(),
        };
        let replies = vec![
            Reply {
                status: 200,
                content_type: "image/jpeg",
                body: "abc".into(),
                extra_header: None,
            },
            Reply {
                status: 200,
                content_type: "image/png",
                body: "abc".into(),
                extra_header: None,
            },
            Reply {
                status: 200,
                content_type: "image/png",
                body: "ab".into(),
                extra_header: None,
            },
        ];
        let (address, _, handle) = server(replies);
        let api = ApiClient::new(&address, None, false).unwrap();
        for _ in 0..3 {
            assert!(matches!(
                api.artwork_bytes(&item),
                Err(ApiError::Protocol(_))
            ));
        }
        handle.join().unwrap();
        let mut oversized = item;
        oversized.size = 8 * 1024 * 1024 + 1;
        assert!(matches!(
            api.artwork_bytes(&oversized),
            Err(ApiError::Protocol(_))
        ));
    }

    #[test]
    fn problem_details_classify_authorization_and_invalid_cursor() {
        let unauthorized = r#"{"type":"urn:app-depot:problem:unauthorized","title":"Authentication required","status":401,"code":"unauthorized"}"#;
        let forbidden = r#"{"type":"urn:app-depot:problem:forbidden","title":"Forbidden","status":403,"code":"forbidden"}"#;
        let invalid = r#"{"type":"urn:app-depot:problem:invalid_cursor","title":"Invalid cursor","status":400,"code":"invalid_cursor"}"#;
        let (address, _, handle) = server(vec![
            Reply {
                status: 401,
                content_type: "application/problem+json",
                body: unauthorized.into(),
                extra_header: None,
            },
            Reply {
                status: 403,
                content_type: "application/problem+json",
                body: forbidden.into(),
                extra_header: None,
            },
            json(r#"{"items":[],"nextCursor":"expired"}"#),
            Reply {
                status: 400,
                content_type: "application/problem+json",
                body: invalid.into(),
                extra_header: None,
            },
        ]);
        let client = ApiClient::new(&address, None, false).unwrap();
        assert!(matches!(
            client.capabilities(),
            Err(ApiError::Authentication(Some(_)))
        ));
        assert!(matches!(
            client.capabilities(),
            Err(ApiError::Authorization(Some(_)))
        ));
        assert!(
            matches!(client.list_apps(1), Err(ApiError::Http { status: 400, problem: Some(Problem { code, .. }) }) if code == "invalid_cursor")
        );
        handle.join().unwrap();
    }

    #[test]
    fn traverses_multiple_pages_and_encodes_opaque_cursor() {
        let first = r#"{"items":[{"id":"a","name":"A","slug":"a","ownerId":"owner","visibility":"Public","state":"Published"}],"nextCursor":"x/+ y"}"#;
        let second = r#"{"items":[{"id":"b","name":"B","slug":"b","ownerId":"owner","visibility":"Public","state":"Published"}],"nextCursor":null}"#;
        let (address, requests, handle) = server(vec![json(first), json(second)]);
        let apps = ApiClient::new(&address, None, false)
            .unwrap()
            .list_apps(1)
            .unwrap();
        assert_eq!(apps.len(), 2);
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[1].contains("cursor=x%2F%2B+y"));
    }

    #[test]
    fn tree_inventory_pages_require_stable_strong_validator_and_order() {
        let id = format!("adtree:v1:sha256:{}", "a".repeat(64));
        let first = r#"{"items":[{"type":"File","path":"a.txt","fileId":"66666666-6666-4666-8666-666666666666","size":18446744073709551615,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}],"nextCursor":"opaque/next"}"#;
        let second = r#"{"items":[{"type":"Directory","path":"empty","fileId":null,"size":null,"sha256":null,"executable":null}],"nextCursor":null}"#;
        let with_etag = |body: &str, etag: &str| Reply {
            status: 200,
            content_type: "application/json",
            body: body.into(),
            extra_header: Some(format!("ETag: {etag}")),
        };
        let (url, requests, handle) = server(vec![
            with_etag(first, "\"same\""),
            with_etag(second, "\"same\""),
        ]);
        let (entries, validator) = ApiClient::new(&url, None, false)
            .unwrap()
            .tree_inventory(&id, 1)
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(validator, "\"same\"");
        assert!(matches!(
            entries[0],
            InventoryEntry::File { size: u64::MAX, .. }
        ));
        handle.join().unwrap();
        assert!(requests.lock().unwrap()[1].contains("cursor=opaque%2Fnext"));

        let (url, _, handle) = server(vec![
            with_etag(first, "\"first\""),
            with_etag(second, "\"second\""),
        ]);
        assert!(matches!(
            ApiClient::new(&url, None, false)
                .unwrap()
                .tree_inventory(&id, 1),
            Err(ApiError::Protocol(_))
        ));
        handle.join().unwrap();
        let second_duplicate = second.replace("\"path\":\"empty\"", "\"path\":\"a.txt\"");
        let (url, _, handle) = server(vec![
            with_etag(first, "\"same\""),
            with_etag(&second_duplicate, "\"same\""),
        ]);
        assert!(matches!(
            ApiClient::new(&url, None, false)
                .unwrap()
                .tree_inventory(&id, 1),
            Err(ApiError::Protocol(_))
        ));
        handle.join().unwrap();
    }

    #[test]
    fn tree_inventory_rejects_malformed_entries_and_cursors() {
        let id = format!("adtree:v1:sha256:{}", "a".repeat(64));
        let cases = [
            r#"{"items":[{"type":"Link","path":"x"}],"nextCursor":null}"#,
            r#"{"items":[{"type":"Directory","path":"x","fileId":"bad"}],"nextCursor":null}"#,
            r#"{"items":[{"type":"File","path":"x","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"x","executable":false,"surprise":1}],"nextCursor":null}"#,
            r#"{"items":[],"nextCursor":"never"}"#,
        ];
        for body in cases {
            let (url, _, handle) = server(vec![Reply {
                status: 200,
                content_type: "application/json",
                body: body.into(),
                extra_header: Some("ETag: \"same\"".into()),
            }]);
            assert!(matches!(
                ApiClient::new(&url, None, false)
                    .unwrap()
                    .tree_inventory(&id, 1),
                Err(ApiError::Protocol(_))
            ));
            handle.join().unwrap();
        }
    }

    #[test]
    fn repeated_cursor_and_wrong_content_type_are_protocol_errors() {
        let page = r#"{"items":[],"nextCursor":"again"}"#;
        let (address, _, handle) = server(vec![
            json(page),
            json(page),
            Reply {
                status: 200,
                content_type: "text/html",
                body: "{}".into(),
                extra_header: None,
            },
        ]);
        let client = ApiClient::new(&address, None, false).unwrap();
        assert!(matches!(client.list_apps(1), Err(ApiError::Protocol(_))));
        assert!(matches!(client.capabilities(), Err(ApiError::Protocol(_))));
        handle.join().unwrap();
    }

    #[test]
    fn snapshot_preserves_all_json_kinds_and_multiple_associations() {
        let body = r#"{"releaseId":"r","app":{"revisionId":"ra","data":{"appDepot":{"summary":"hello","new":18446744073709551616},"other":[null,true,18446744073709551615]}},"release":{"revisionId":"rr","data":[1,"text",null]},"associations":[{"associationId":"x","declaration":{"revisionId":"rx","data":{"appDepot":{"entryPoints":[{"id":"run","target":{"type":"ArtifactFile"},"future":true}]}}}},{"associationId":"y","declaration":{"revisionId":"ry","data":null}}]}"#;
        let (address, _, handle) = server(vec![json(body)]);
        let snapshot = ApiClient::new(&address, None, false)
            .unwrap()
            .declarations("r")
            .unwrap();
        assert_eq!(snapshot.associations.len(), 2);
        assert_eq!(
            snapshot.app.data["appDepot"]["new"].to_string(),
            "18446744073709551616"
        );
        assert_eq!(snapshot.app.data["other"][2].as_u64(), Some(u64::MAX));
        assert!(snapshot.release.data.is_array());
        assert!(snapshot.associations[1].declaration.data.is_null());
        let known: AppConvention = snapshot.app.convention().unwrap().unwrap();
        assert_eq!(known.summary.as_deref(), Some("hello"));
        let known: AssociationConvention = snapshot.associations[0]
            .declaration
            .convention()
            .unwrap()
            .unwrap();
        assert_eq!(known.entry_points.unwrap()[0].id, "run");
        let known: Result<Option<ReleaseConvention>, _> = snapshot.release.convention();
        assert!(known.is_ok_and(|value| value.is_none()));
        let printed = serde_json::to_string(&snapshot).unwrap();
        assert!(printed.contains("18446744073709551616"));
        handle.join().unwrap();
    }

    #[test]
    fn all_associations_resolve_to_distinct_artifact_metadata() {
        let file_id = format!("adfile:sha256:{}", "a".repeat(64));
        let tree_id = format!("adtree:v1:sha256:{}", "b".repeat(64));
        let first_page = format!(
            r#"{{"items":[{{"id":"x","releaseId":"r","artifactId":"{file_id}"}}],"nextCursor":"next"}}"#
        );
        let second_page = format!(
            r#"{{"items":[{{"id":"y","releaseId":"r","artifactId":"{tree_id}"}}],"nextCursor":null}}"#
        );
        let file = format!(r#"{{"id":"{file_id}","kind":"File","size":18446744073709551615}}"#);
        let tree = format!(r#"{{"id":"{tree_id}","kind":"Tree","size":null}}"#);
        let (address, requests, handle) = server(vec![
            json(&first_page),
            json(&second_page),
            json(&file),
            json(&tree),
        ]);
        let items = ApiClient::new(&address, None, false)
            .unwrap()
            .associated_artifacts("r", 1)
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].artifact.size, Some(u64::MAX));
        assert_eq!(items[1].artifact.size, None);
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[2].starts_with(&format!("GET /api/v1/artifacts/{file_id} HTTP/1.1")));
    }

    #[test]
    fn redirects_never_forward_credentials() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_url = format!("http://{}/steal", target.local_addr().unwrap());
        let (address, requests, handle) = server(vec![Reply {
            status: 302,
            content_type: "text/plain",
            body: String::new(),
            extra_header: Some(format!("Location: {target_url}")),
        }]);
        let error = ApiClient::new(&address, Some("secret".into()), false)
            .unwrap()
            .capabilities()
            .unwrap_err();
        assert!(matches!(error, ApiError::Protocol(_)));
        handle.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(target.accept().is_err());
    }

    #[test]
    fn missing_malformed_and_oversized_responses_fail() {
        let (address, _, handle) = server(vec![
            json("{}"),
            json("not json"),
            Reply {
                status: 200,
                content_type: "application/json",
                body: "x".repeat(MAX_JSON_BYTES as usize + 1),
                extra_header: None,
            },
        ]);
        let client = ApiClient::new(&address, None, false).unwrap();
        assert!(matches!(client.capabilities(), Err(ApiError::Protocol(_))));
        assert!(matches!(client.capabilities(), Err(ApiError::Protocol(_))));
        assert!(matches!(client.capabilities(), Err(ApiError::Protocol(_))));
        handle.join().unwrap();
    }

    #[test]
    fn rejects_unsafe_urls_and_encodes_path_segments() {
        for url in [
            "file:///tmp",
            "https://user:pass@example.org/",
            "https://example.org/api",
            "http://example.org/",
        ] {
            assert!(ApiClient::new(url, None, false).is_err());
        }
        let client = ApiClient::new("http://127.0.0.1:8787/", None, false).unwrap();
        assert!(
            client
                .url(&["apps", "a/b"], None, None)
                .unwrap()
                .as_str()
                .ends_with("/api/v1/apps/a%2Fb")
        );
    }
}
