//! Read-only installation plan metadata. No execution or materialization.
use crate::{
    ServerProfileId,
    api::ApiClient,
    library::{Library, LibraryError},
    models::{DeclarationDocument, InventoryEntry},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

pub type Selections = BTreeMap<String, Vec<String>>;
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRequest {
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    pub selections: Selections,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub recipe_payload: bool,
    #[serde(default)]
    pub payload: Vec<Copy>,
    #[serde(default)]
    pub options: Vec<OptionGroup>,
    pub localization: Option<Localization>,
    #[serde(default)]
    pub prerequisites: Vec<DeclaredPrerequisite>,
    pub launch: Option<DeclaredLaunch>,
    #[serde(default)]
    pub unresolved: Vec<String>,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Copy {
    pub association_id: String,
    #[serde(default)]
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub collision: Collision,
    #[serde(default)]
    pub platforms: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptionGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub language: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub defaults: Vec<String>,
    pub choices: Vec<Choice>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub payload: Vec<Copy>,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum Localization {
    Fixed { language: String },
    Bundled { languages: Vec<String> },
    Selectable,
    Unknown,
}
#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Collision {
    #[default]
    Reject,
    ReplaceFile,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredPrerequisite {
    pub id: String,
    pub name: String,
    pub association_id: String,
    #[serde(default)]
    pub path: String,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    pub evidence: String,
    #[serde(default)]
    pub when: Option<Condition>,
    #[serde(default = "success_codes")]
    pub success_exit_codes: Vec<i32>,
}
fn success_codes() -> Vec<i32> {
    vec![0]
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    pub option_id: String,
    pub choice_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredLaunch {
    pub association_id: String,
    pub entry_point_id: String,
    pub revision_id: String,
    pub destination: String,
    pub working_directory: Option<String>,
}
fn label(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn distinct<T: Eq + std::hash::Hash>(v: &[T]) -> bool {
    v.iter().collect::<HashSet<_>>().len() == v.len()
}
fn platforms(v: &[String]) -> bool {
    v.len() <= 6 && distinct(v) && v.iter().all(|p| platform(p))
}
fn language(s: &str) -> bool {
    s.len() <= 80
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && s.split('-')
            .all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}
pub fn manifest(doc: &DeclarationDocument) -> Result<Option<Manifest>, LibraryError> {
    let Some(raw) = doc.app_depot().and_then(|v| v.get("installation")) else {
        return Ok(None);
    };
    if raw.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid(
            if raw.get("version").and_then(Value::as_u64).is_some() {
                "unsupported installation manifest version"
            } else {
                "invalid installation manifest version"
            },
        ));
    }
    let m: Manifest = serde_json::from_value(raw.clone())?;
    if m.options.len() > 16
        || m.prerequisites.len() > 32
        || m.unresolved.len() > 128
        || m.payload.len()
            + m.options
                .iter()
                .flat_map(|g| &g.choices)
                .map(|c| c.payload.len())
                .sum::<usize>()
            > 128
        || (!m.recipe_payload && m.payload.is_empty())
    {
        return Err(invalid("invalid manifest limits"));
    }
    if m.recipe_payload
        && (!m.payload.is_empty()
            || !m.options.is_empty()
            || !m.prerequisites.is_empty()
            || m.launch.is_some())
        || m.unresolved.iter().any(|s| !label(s, 1024))
    {
        return Err(invalid("invalid manifest content"));
    }
    if m.payload
        .iter()
        .chain(
            m.options
                .iter()
                .flat_map(|g| g.choices.iter().flat_map(|c| &c.payload)),
        )
        .any(|c| {
            !uuid(&c.association_id)
                || !path(&c.source, true)
                || !path(&c.destination, true)
                || !platforms(&c.platforms)
        })
    {
        return Err(invalid("invalid manifest copy"));
    }
    let mut groups = HashSet::new();
    for g in &m.options {
        if !identifier(&g.id)
            || !groups.insert(&g.id)
            || !label(&g.name, 160)
            || g.choices.is_empty()
            || g.choices.len() > 32
            || !distinct(&g.defaults)
            || (!g.multiple && g.defaults.len() > 1)
            || g.defaults
                .iter()
                .any(|id| !g.choices.iter().any(|c| &c.id == id))
            || g.choices
                .iter()
                .map(|c| c.id.to_ascii_lowercase())
                .collect::<HashSet<_>>()
                .len()
                != g.choices.len()
            || g.choices.iter().any(|c| {
                !identifier(&c.id) || !label(&c.name, 160) || (g.language && !language(&c.id))
            })
        {
            return Err(invalid("invalid manifest option group"));
        }
    }
    let languages: Vec<_> = m.options.iter().filter(|g| g.language).collect();
    if languages.len() > 1
        || languages.first().is_some_and(|g| g.multiple)
        || match &m.localization {
            None => false,
            Some(Localization::Fixed { language: tag }) => !languages.is_empty() || !language(tag),
            Some(Localization::Bundled { languages: tags }) => {
                !languages.is_empty()
                    || !(2..=32).contains(&tags.len())
                    || tags.iter().any(|t| !language(t))
                    || tags
                        .iter()
                        .map(|t| t.to_ascii_lowercase())
                        .collect::<HashSet<_>>()
                        .len()
                        != tags.len()
            }
            Some(Localization::Unknown) => !languages.is_empty(),
            Some(Localization::Selectable) => languages.first().is_none_or(|g| {
                g.choices.len() < 2 || !g.choices.windows(2).any(|w| w[0].payload != w[1].payload)
            }),
        }
    {
        return Err(invalid("invalid localization"));
    }
    let mut ids = HashSet::new();
    for p in &m.prerequisites {
        if !identifier(&p.id)
            || !ids.insert(&p.id)
            || !label(&p.name, 160)
            || !label(&p.evidence, 1024)
            || !uuid(&p.association_id)
            || !path(&p.path, true)
            || p.platforms.is_empty()
            || !platforms(&p.platforms)
            || !args_valid(&p.arguments)
            || !(1..=16).contains(&p.success_exit_codes.len())
            || !distinct(&p.success_exit_codes)
            || p.when.as_ref().is_some_and(|c| {
                !m.options
                    .iter()
                    .any(|g| g.id == c.option_id && g.choices.iter().any(|v| v.id == c.choice_id))
            })
        {
            return Err(invalid("invalid declared prerequisite"));
        }
    }
    if m.launch.as_ref().is_some_and(|l| {
        !uuid(&l.association_id)
            || !uuid(&l.revision_id)
            || !identifier(&l.entry_point_id)
            || !path(&l.destination, false)
            || l.working_directory.as_ref().is_some_and(|p| !path(p, true))
    }) {
        return Err(invalid("invalid declared launch"));
    }
    Ok(Some(m))
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub association_id: String,
    pub artifact_id: String,
    pub file_id: Option<String>,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prerequisite {
    pub id: String,
    pub name: String,
    pub file: Source,
    pub path: String,
    pub arguments: Vec<String>,
    pub optional: bool,
    pub evidence: String,
    pub success_exit_codes: Vec<i32>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub method: String,
    pub association_id: String,
    pub entry_point_id: String,
    pub revision_id: String,
    pub path: String,
    pub working_directory: String,
    pub arguments: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub app_id: String,
    pub release_id: String,
    pub revision_id: String,
    pub platform: String,
    pub selections: Selections,
    pub localization: Option<Localization>,
    pub unresolved: Vec<String>,
    pub files: Option<BTreeMap<String, Source>>,
    pub directories: Option<Vec<String>>,
    pub prerequisites: Option<Vec<Prerequisite>>,
    pub launch: Option<Launch>,
    pub recipe: Option<Value>,
}
fn invalid(s: &str) -> LibraryError {
    LibraryError::Invalid(s.into())
}
fn uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|v| v.to_string() == s)
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn path(s: &str, empty: bool) -> bool {
    (empty && s.is_empty())
        || (!s.is_empty()
            && s.len() <= 1024
            && s.split('/').count() <= 32
            && !s.contains(['\\', ':'])
            && !s.chars().any(char::is_control)
            && s.split('/').all(|c| !matches!(c, "" | "." | "..")))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn args_valid(a: &[String]) -> bool {
    a.len() <= 64
        && a.iter().map(String::len).sum::<usize>() <= 16384
        && a.iter()
            .all(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
}
fn platform(s: &str) -> bool {
    matches!(
        s,
        "windows/x86_64"
            | "windows/aarch64"
            | "linux/x86_64"
            | "linux/aarch64"
            | "macos/x86_64"
            | "macos/aarch64"
    )
}
impl Plan {
    pub fn parse(
        raw: &Value,
        release: &str,
        request: &ResolveRequest,
    ) -> Result<Self, LibraryError> {
        let p: Self = serde_json::from_value(raw.clone())?;
        if p.version != 1
            || !uuid(&p.app_id)
            || !uuid(&p.release_id)
            || !uuid(&p.revision_id)
            || p.release_id != release
            || p.platform != request.platform
            || !platform(&p.platform)
            || request
                .revision_id
                .as_ref()
                .is_some_and(|r| r != &p.revision_id)
            || p.selections.len() > 16
            || p.selections.iter().any(|(k, v)| {
                !identifier(k)
                    || v.len() > 32
                    || v.iter().any(|c| !identifier(c))
                    || v.iter().collect::<HashSet<_>>().len() != v.len()
            })
            || request.selections.iter().any(|(k, v)| {
                p.selections.get(k).is_none_or(|s| {
                    s.iter().collect::<HashSet<_>>() != v.iter().collect::<HashSet<_>>()
                })
            })
            || p.unresolved.len() > 128
            || p.unresolved.iter().any(|s| s.is_empty() || s.len() > 1024)
        {
            return Err(invalid("invalid plan identity, selections or limits"));
        }
        if let Some(recipe) = &p.recipe {
            if recipe["version"].as_u64() != Some(1)
                || recipe["releaseId"].as_str() != Some(&p.release_id)
                || recipe["appId"].as_str() != Some(&p.app_id)
                || recipe["platform"].as_str() != Some(&p.platform)
                || recipe["releases"]
                    .as_object()
                    .is_none_or(|r| r.len() > 128 || !r.contains_key(&p.release_id))
                || recipe["files"]
                    .as_object()
                    .zip(recipe["directories"].as_array())
                    .is_none_or(|(f, d)| f.len() + d.len() > 50000)
                || !recipe["launch"].is_object()
            {
                return Err(invalid("invalid Derived lock envelope"));
            }
            if ["files", "directories", "prerequisites", "launch"]
                .iter()
                .any(|k| raw.get(k).is_some())
            {
                return Err(invalid("mixed Derived and Standard plan"));
            }
            return Ok(p);
        }
        let (Some(files), Some(dirs), Some(prereqs)) = (&p.files, &p.directories, &p.prerequisites)
        else {
            return Err(invalid("missing Standard plan fields"));
        };
        if raw.get("recipe").is_some()
            || raw.get("launch").is_none()
            || files.len() + dirs.len() > 50000
            || prereqs.len() > 32
        {
            return Err(invalid("invalid plan limits"));
        }
        let dirset: HashSet<_> = dirs.iter().map(String::as_str).collect();
        if dirset.len() != dirs.len() || dirs.windows(2).any(|w| w[0] >= w[1]) {
            return Err(invalid("invalid directory order"));
        }
        if raw["files"]
            .as_object()
            .is_none_or(|f| f.values().any(|v| v.get("fileId").is_none()))
            || raw["prerequisites"]
                .as_array()
                .is_none_or(|p| p.iter().any(|v| v["file"].get("fileId").is_none()))
        {
            return Err(invalid("missing source fileId"));
        }
        let mut inventory = Vec::new();
        for d in dirs {
            inventory.push(InventoryEntry::Directory { path: d.clone() });
        }
        for (dest, file) in files {
            if dirset.contains(dest.as_str()) {
                return Err(invalid("file-directory collision"));
            }
            file.validate()?;
            inventory.push(InventoryEntry::File {
                path: dest.clone(),
                file_id: file.file_id.clone().unwrap_or_default(),
                size: file.size,
                sha256: file.sha256.clone(),
                executable: file.executable,
            });
        }
        for e in &inventory {
            if !path(e.path(), false)
                || e.path()
                    .rsplit_once('/')
                    .is_some_and(|(parent, _)| !dirset.contains(parent))
            {
                return Err(invalid("invalid destination path or missing parent"));
            }
        }
        if p.platform.starts_with("windows/") {
            crate::tree::validate_destination(&inventory)?;
        }
        let mut ids = HashSet::new();
        for pre in prereqs {
            pre.file.validate()?;
            if !identifier(&pre.id)
                || !ids.insert(&pre.id)
                || !path(&pre.path, pre.file.file_id.is_none())
                || !args_valid(&pre.arguments)
                || pre.name.trim().is_empty()
                || pre.name.len() > 160
                || pre.evidence.trim().is_empty()
                || pre.evidence.len() > 1024
                || pre.success_exit_codes.is_empty()
                || pre.success_exit_codes.len() > 16
                || pre.success_exit_codes.iter().collect::<HashSet<_>>().len()
                    != pre.success_exit_codes.len()
            {
                return Err(invalid("invalid prerequisite metadata"));
            }
        }
        if let Some(l) = &p.launch
            && (l.method != "NativePortableV1"
                || !identifier(&l.entry_point_id)
                || !uuid(&l.revision_id)
                || !args_valid(&l.arguments)
                || files.get(&l.path).is_none_or(|f| {
                    f.association_id != l.association_id
                        || (!p.platform.starts_with("windows/") && !f.executable)
                })
                || (!l.working_directory.is_empty()
                    && !dirset.contains(l.working_directory.as_str())))
        {
            return Err(invalid("invalid launch reference"));
        }
        Ok(p)
    }
}
impl Source {
    fn validate(&self) -> Result<(), LibraryError> {
        if !uuid(&self.association_id)
            || !digest(&self.sha256)
            || self.file_id.as_ref().is_some_and(|id| !uuid(id))
            || match &self.file_id {
                None => self.artifact_id != format!("adfile:sha256:{}", self.sha256),
                Some(_) => !self
                    .artifact_id
                    .strip_prefix("adtree:v1:sha256:")
                    .is_some_and(digest),
            }
        {
            return Err(invalid("invalid source identity"));
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedPlan {
    pub id: String,
    pub profile_id: ServerProfileId,
    pub fingerprint: String,
    pub plan: Value,
    pub assessment: String,
}
impl Library {
    pub fn resolve_plan(
        &self,
        profile: &ServerProfileId,
        client: &ApiClient,
        release: &str,
        request: &ResolveRequest,
    ) -> Result<SavedPlan, LibraryError> {
        if self.profile(profile)?.origin != client.origin() {
            return Err(invalid("profile origin mismatch"));
        }
        if !uuid(release)
            || !platform(&request.platform)
            || request.revision_id.as_ref().is_some_and(|r| !uuid(r))
        {
            return Err(invalid("invalid resolver request"));
        }
        let caps = client.ensure_read_compatible()?;
        let page = caps.limits.max_page_size;
        let raw = client.resolve_installation(release, request)?;
        let plan = Plan::parse(&raw, release, request)?;
        if client.release(release)?.app_id != plan.app_id {
            return Err(invalid("plan App mismatch"));
        }
        let doc = client.release_declaration_revision(release, &plan.revision_id)?;
        let manifest = manifest(&doc)?
            .ok_or_else(|| invalid("resolved revision has no installation manifest"))?;
        if manifest.recipe_payload != plan.recipe.is_some()
            || manifest.localization != plan.localization
            || manifest.unresolved != plan.unresolved
        {
            return Err(invalid("plan differs from pinned manifest"));
        }
        let mut normalized = Selections::new();
        for group in &manifest.options {
            let selected = request.selections.get(&group.id).unwrap_or(&group.defaults);
            let ordered: Vec<_> = group
                .choices
                .iter()
                .filter(|c| selected.contains(&c.id))
                .map(|c| c.id.clone())
                .collect();
            if ordered.len() != selected.len()
                || (group.required && ordered.is_empty())
                || (!group.multiple && ordered.len() > 1)
                || normalized.insert(group.id.clone(), ordered).is_some()
            {
                return Err(invalid("invalid selected options"));
            }
        }
        if normalized != plan.selections {
            return Err(invalid("normalized selections mismatch"));
        }
        if let Some(prereqs) = &plan.prerequisites {
            let expected: Vec<_> = manifest
                .prerequisites
                .iter()
                .filter(|p| {
                    p.platforms.contains(&plan.platform)
                        && p.when.as_ref().is_none_or(|c| {
                            plan.selections
                                .get(&c.option_id)
                                .is_some_and(|s| s.contains(&c.choice_id))
                        })
                })
                .collect();
            if expected.len() != prereqs.len()
                || expected.iter().zip(prereqs).any(|(a, b)| {
                    a.id != b.id
                        || a.name != b.name
                        || a.association_id != b.file.association_id
                        || a.path != b.path
                        || a.arguments != b.arguments
                        || a.optional != b.optional
                        || a.evidence != b.evidence
                        || a.success_exit_codes != b.success_exit_codes
                })
            {
                return Err(invalid("prerequisites differ from pinned declaration"));
            }
        }
        if let Some(launch) = &plan.launch
            && manifest.launch.as_ref().is_none_or(|l| {
                l.association_id != launch.association_id
                    || l.revision_id != launch.revision_id
                    || l.entry_point_id != launch.entry_point_id
                    || l.destination != launch.path
                    || l.working_directory
                        .as_ref()
                        .is_some_and(|w| w != &launch.working_directory)
            })
        {
            return Err(invalid("launch differs from pinned manifest"));
        }
        let selected_associations: HashSet<_> = manifest
            .payload
            .iter()
            .chain(manifest.options.iter().flat_map(|g| {
                g.choices
                    .iter()
                    .filter(|c| {
                        plan.selections
                            .get(&g.id)
                            .is_some_and(|s| s.contains(&c.id))
                    })
                    .flat_map(|c| &c.payload)
            }))
            .filter(|c| c.platforms.is_empty() || c.platforms.contains(&plan.platform))
            .map(|c| &c.association_id)
            .collect();
        if plan.files.as_ref().is_some_and(|f| {
            f.values()
                .any(|s| !selected_associations.contains(&s.association_id))
        }) {
            return Err(invalid("plan source not selected by manifest"));
        }
        let mut trees = HashMap::new();
        if let Some(files) = &plan.files {
            let associations: HashMap<_, _> = client
                .list_associations(release, page)?
                .into_iter()
                .map(|a| (a.id, a.artifact_id))
                .collect();
            let mut artifacts = HashMap::new();
            for (source, original) in files.values().map(|f| (f, None)).chain(
                plan.prerequisites
                    .iter()
                    .flatten()
                    .map(|p| (&p.file, Some(p.path.as_str()))),
            ) {
                if associations.get(&source.association_id) != Some(&source.artifact_id) {
                    return Err(invalid("source association mismatch"));
                }
                if !artifacts.contains_key(&source.artifact_id) {
                    artifacts.insert(
                        source.artifact_id.clone(),
                        client.artifact(&source.artifact_id)?,
                    );
                }
                if let Some(file_id) = &source.file_id {
                    if !trees.contains_key(&source.artifact_id) {
                        let (entries, _) = client.tree_inventory(&source.artifact_id, page)?;
                        if entries.len() > 50000
                            || crate::tree::identity(&entries)? != source.artifact_id
                        {
                            return Err(invalid("invalid source Tree identity"));
                        }
                        let members: HashMap<_, _> = entries
                            .into_iter()
                            .filter_map(|e| match e {
                                InventoryEntry::File {
                                    file_id,
                                    path,
                                    size,
                                    sha256,
                                    executable,
                                } => Some((file_id, (path, size, sha256, executable))),
                                _ => None,
                            })
                            .collect();
                        trees.insert(source.artifact_id.clone(), members);
                    }
                    if trees[&source.artifact_id].get(file_id).is_none_or(
                        |(path, size, hash, exec)| {
                            *size != source.size
                                || hash != &source.sha256
                                || *exec != source.executable
                                || original.is_some_and(|p| p != path)
                        },
                    ) {
                        return Err(invalid("source Tree member mismatch"));
                    }
                } else if artifacts[&source.artifact_id].size != Some(source.size)
                    || original.is_some_and(|p| !p.is_empty())
                {
                    return Err(invalid("source File mismatch"));
                }
            }
        }
        if let Some(declared) = &manifest.launch {
            let data = client.association_declaration_revision(
                &declared.association_id,
                &declared.revision_id,
            )?;
            let entry = data
                .pointer("/appDepot/entryPoints")
                .and_then(Value::as_array)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|e| e["id"].as_str() == Some(&declared.entry_point_id))
                })
                .ok_or_else(|| invalid("launch entry point missing"))?;
            let applicable = entry["platforms"]
                .as_array()
                .is_some_and(|p| p.iter().any(|v| v.as_str() == Some(&plan.platform)));
            if applicable != plan.launch.is_some() {
                return Err(invalid("launch platform mismatch"));
            }
            if let Some(launch) = &plan.launch {
                let args: Vec<String> = entry
                    .pointer("/launch/arguments")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()?
                    .unwrap_or_default();
                if entry.pointer("/launch/method").and_then(Value::as_str)
                    != Some("NativePortableV1")
                    || args != launch.arguments
                {
                    return Err(invalid("launch declaration mismatch"));
                }
                let expected_work = declared
                    .working_directory
                    .as_deref()
                    .or_else(|| {
                        entry
                            .pointer("/launch/workingDirectory")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or_else(|| {
                        launch
                            .path
                            .rsplit_once('/')
                            .map_or("", |(parent, _)| parent)
                    });
                if expected_work != launch.working_directory {
                    return Err(invalid("launch working directory mismatch"));
                }
                let source = &plan.files.as_ref().unwrap()[&launch.path];
                match entry.pointer("/target/type").and_then(Value::as_str) {
                    Some("ArtifactFile") if source.file_id.is_none() => {}
                    Some("TreeFile") if source.file_id.is_some() => {
                        let file_id = source.file_id.as_ref().unwrap();
                        if trees
                            .get(&source.artifact_id)
                            .and_then(|members| members.get(file_id))
                            .is_none_or(|(path, _, _, _)| {
                                entry.pointer("/target/path").and_then(Value::as_str) != Some(path)
                            })
                        {
                            return Err(invalid("launch Tree target mismatch"));
                        }
                    }
                    _ => return Err(invalid("launch target mismatch")),
                }
            }
        }
        let json = serde_json::to_string(&raw)?;
        let saved = SavedPlan {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile.clone(),
            fingerprint: format!("{:x}", Sha256::digest(json.as_bytes())),
            assessment: if plan.recipe.is_some() {
                "Derived metadata only".into()
            } else {
                "Validated Standard metadata; execution unsupported".into()
            },
            plan: raw,
        };
        self.conn.execute("INSERT INTO installation_plans(id,profile_id,plan_json,fingerprint) VALUES (?1,?2,?3,?4)",rusqlite::params![saved.id,profile.0,json,saved.fingerprint])?;
        Ok(saved)
    }
    pub fn saved_plans(&self, profile: &ServerProfileId) -> Result<Vec<SavedPlan>, LibraryError> {
        let mut stmt=self.conn.prepare("SELECT id,plan_json,fingerprint FROM installation_plans WHERE profile_id=?1 ORDER BY id")?;
        let rows = stmt.query_map([&profile.0], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (id, json, fingerprint) = r?;
            if format!("{:x}", Sha256::digest(json.as_bytes())) != fingerprint {
                return Err(invalid("saved plan fingerprint mismatch"));
            }
            Ok(SavedPlan {
                id,
                profile_id: profile.clone(),
                fingerprint,
                assessment: if serde_json::from_str::<Value>(&json)?
                    .get("recipe")
                    .is_some()
                {
                    "Derived metadata only".into()
                } else {
                    "Validated Standard metadata; execution unsupported".into()
                },
                plan: serde_json::from_str(&json)?,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const RELEASE: &str = "11111111-1111-4111-8111-111111111111";
    const REVISION: &str = "22222222-2222-4222-8222-222222222222";
    const APP: &str = "33333333-3333-4333-8333-333333333333";
    const ASSOCIATION: &str = "44444444-4444-4444-8444-444444444444";
    fn request() -> ResolveRequest {
        ResolveRequest {
            platform: "windows/x86_64".into(),
            revision_id: Some(REVISION.into()),
            selections: Default::default(),
        }
    }
    fn raw_plan() -> Value {
        let hash = "a".repeat(64);
        json!({"version":1,"releaseId":RELEASE,"appId":APP,"revisionId":REVISION,"platform":"windows/x86_64","selections":{},"unresolved":[],"files":{"game/data":{"associationId":ASSOCIATION,"artifactId":format!("adfile:sha256:{hash}"),"fileId":null,"sha256":hash,"size":42,"executable":false}},"directories":["game"],"prerequisites":[],"launch":null})
    }
    fn doc(value: Value) -> DeclarationDocument {
        DeclarationDocument {
            revision_id: REVISION.into(),
            data: value,
            unknown: Default::default(),
        }
    }
    #[test]
    fn manifest_absent_invalid_and_unsupported() {
        assert!(manifest(&doc(json!({}))).unwrap().is_none());
        for v in [json!(null), json!({"version":2}), json!({"version":1})] {
            assert!(manifest(&doc(json!({"appDepot":{"installation":v}}))).is_err());
        }
    }
    #[test]
    fn localization_modes() {
        for mode in [
            json!({"mode":"fixed","language":"ru-RU"}),
            json!({"mode":"bundled","languages":["en-US","ru-RU"]}),
            json!({"mode":"unknown"}),
        ] {
            assert!(manifest(&doc(json!({"appDepot":{"installation":{"version":1,"payload":[{"associationId":ASSOCIATION,"destination":""}],"localization":mode}}}))).unwrap().is_some());
        }
        let manifest_doc = doc(
            json!({"appDepot":{"installation":{"version":1,"payload":[{"associationId":ASSOCIATION,"destination":""}],"localization":{"mode":"selectable"},"options":[{"id":"language","name":"Language","language":true,"required":true,"defaults":["en-US"],"choices":[{"id":"en-US","name":"English","payload":[]},{"id":"ru-RU","name":"Russian","payload":[{"associationId":ASSOCIATION,"source":"ru","destination":"data"}]}]}]}}}),
        );
        let m = manifest(&manifest_doc).unwrap().unwrap();
        assert_eq!(m.options[0].defaults, ["en-US"]);
        assert!(m.options[0].required);
    }
    #[test]
    fn plan_rejects_identity_source_and_path_mismatches() {
        let raw = raw_plan();
        assert!(Plan::parse(&raw, RELEASE, &request()).is_ok());
        for (key, value) in [
            ("revisionId", json!(APP)),
            ("platform", json!("linux/x86_64")),
            ("directories", json!([])),
            ("directories", json!(["..", "game"])),
        ] {
            let mut changed = raw.clone();
            changed[key] = value;
            assert!(Plan::parse(&changed, RELEASE, &request()).is_err());
        }
        let mut changed = raw.clone();
        changed["files"]["game/data"]["sha256"] = json!("bad");
        assert!(Plan::parse(&changed, RELEASE, &request()).is_err());
    }
    #[test]
    fn derived_is_inert_and_cannot_mix_standard_fields() {
        let mut raw = raw_plan();
        raw["recipe"] = json!({"version":1,"releaseId":RELEASE,"appId":APP,"platform":"windows/x86_64","releases":{(RELEASE):{"appId":APP,"version":null,"dependencies":{},"artifacts":{}}},"files":{},"directories":[],"launch":{}});
        assert!(Plan::parse(&raw, RELEASE, &request()).is_err());
        for field in ["files", "directories", "prerequisites", "launch"] {
            raw.as_object_mut().unwrap().remove(field);
        }
        assert!(
            Plan::parse(&raw, RELEASE, &request())
                .unwrap()
                .recipe
                .is_some()
        );
    }
    #[test]
    fn large_plan_is_readable() {
        let mut raw = raw_plan();
        let sample = raw["files"]["game/data"].clone();
        let files = raw["files"].as_object_mut().unwrap();
        files.clear();
        for i in 0..6023 {
            files.insert(format!("game/data-{i:05}"), sample.clone());
        }
        assert_eq!(
            Plan::parse(&raw, RELEASE, &request())
                .unwrap()
                .files
                .unwrap()
                .len(),
            6023
        );
    }
}
