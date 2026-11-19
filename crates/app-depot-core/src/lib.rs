//! UI-independent foundation for App Depot client identities.
//!
//! HTTP catalog, local metadata, and verified file transfer live here.

pub mod api;
pub mod launch;
pub mod library;
pub mod models;
pub mod secrets;
pub mod transfer;
pub mod tree;
pub mod tree_download;

/// Locally generated identity for a configured Server profile. A URL is not an identity:
/// endpoints may move, and different profiles may use the same URL with different credentials.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerProfileId(pub String);

/// Server-local App UUID scoped by the client's configured Server profile.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AppKey {
    pub server: ServerProfileId,
    pub app_id: String,
}

/// A Release is distinct from its App, visible version, and associated Artifacts.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReleaseKey {
    pub server: ServerProfileId,
    pub release_id: String,
}

/// An association supplies Release-specific context for immutable Artifact bytes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AssociationKey {
    pub server: ServerProfileId,
    pub association_id: String,
}

/// Typed content identity scoped by a Server profile for local ownership and access.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ArtifactKey {
    pub server: ServerProfileId,
    pub artifact_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_remote_ids_on_distinct_profiles_are_distinct_local_keys() {
        let first = AppKey {
            server: ServerProfileId("a".into()),
            app_id: "same".into(),
        };
        let second = AppKey {
            server: ServerProfileId("b".into()),
            app_id: "same".into(),
        };
        assert_ne!(first, second);
    }
}
