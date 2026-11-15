//! Models of the Server's current OpenAPI read responses. Declarations retain raw JSON.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub api_version: String,
    pub ingestion_modes: Vec<String>,
    pub zip_methods: Vec<String>,
    pub tree_identity: String,
    pub file_identity: String,
    pub upload_ranges: bool,
    pub download_ranges: bool,
    pub limits: Limits,
    pub features: Vec<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_page_size: u64,
    pub max_concurrent_ranges: u64,
    pub max_upload_bytes: u64,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl Capabilities {
    /// Issues affecting the documented M1 transfer protocol, reported even though
    /// this read-only milestone does not perform downloads.
    pub fn incompatibilities(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.api_version != "1.0.0" {
            issues.push(format!(
                "unreviewed API version {} (implemented 1.0.0)",
                self.api_version
            ));
        }
        if self.file_identity != "adfile:sha256" {
            issues.push(format!("unsupported file identity {}", self.file_identity));
        }
        if self.tree_identity != "adtree:v1:sha256" {
            issues.push(format!("unsupported tree identity {}", self.tree_identity));
        }
        if !self.download_ranges {
            issues.push("download Range support is unavailable".into());
        }
        if !(1..=1000).contains(&self.limits.max_page_size) {
            issues.push(format!("invalid maxPageSize {}", self.limits.max_page_size));
        }
        if self.limits.max_concurrent_ranges == 0 {
            issues.push("invalid maxConcurrentRanges 0".into());
        }
        if self.limits.max_upload_bytes == 0 {
            issues.push("invalid maxUploadBytes 0".into());
        }
        issues
    }

    pub fn page_size(&self) -> Result<u64, String> {
        if (1..=1000).contains(&self.limits.max_page_size) {
            Ok(self.limits.max_page_size.min(100))
        } else {
            Err(format!("invalid maxPageSize {}", self.limits.max_page_size))
        }
    }

    pub fn read_page_size(&self) -> Result<u64, String> {
        if self.api_version != "1.0.0" {
            return Err(format!(
                "unreviewed API version {} (implemented 1.0.0)",
                self.api_version
            ));
        }
        self.page_size()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub owner_id: String,
    pub visibility: String,
    pub state: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub id: String,
    pub app_id: String,
    pub state: String,
    pub version: Option<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Association {
    pub id: String,
    pub release_id: String,
    pub artifact_id: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum InventoryEntry {
    File {
        path: String,
        #[serde(rename = "fileId")]
        file_id: String,
        size: u64,
        sha256: String,
        executable: bool,
    },
    Directory {
        path: String,
    },
}

impl InventoryEntry {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path } => path,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub kind: String,
    pub size: Option<u64>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationDocument {
    pub revision_id: String,
    pub data: Value,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl DeclarationDocument {
    /// Returns only the currently standardized optional convention. Other data
    /// remains available in `data` without rewriting it or inventing semantics.
    pub fn app_depot(&self) -> Option<&Value> {
        self.data.as_object()?.get("appDepot")
    }

    /// A lossy, read-only view of known `appDepot` fields. The full `data` is
    /// always retained, and malformed or future conventions remain inert.
    pub fn convention<T: DeserializeOwned>(&self) -> Result<Option<T>, String> {
        match self.app_depot() {
            None => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| error.to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConvention {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub license: Option<String>,
    pub homepage_url: Option<String>,
    pub tags: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseConvention {
    pub notes: Option<String>,
    pub platforms: Option<Vec<String>>,
    pub runtime: Option<RuntimeConvention>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuntimeConvention {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationConvention {
    pub label: Option<String>,
    pub purpose: Option<String>,
    pub description: Option<String>,
    pub entry_points: Option<Vec<EntryPointConvention>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EntryPointConvention {
    pub id: String,
    pub label: Option<String>,
    pub target: EntryPointTarget,
    pub platforms: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum EntryPointTarget {
    ArtifactFile,
    TreeFile { path: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotAssociation {
    pub association_id: String,
    pub declaration: DeclarationDocument,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationSnapshot {
    pub release_id: String,
    pub app: DeclarationDocument,
    pub release: DeclarationDocument,
    pub associations: Vec<SnapshotAssociation>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssociationArtifact {
    pub association: Association,
    pub artifact: Artifact,
}
