//! Read-only compatibility check against an isolated Dev/Test Server.

use app_depot_core::{
    api::ApiClient,
    library::{Library, LibraryPaths},
};
use std::env;

#[test]
#[ignore = "requires isolated Dev/Test Server and published App/Release fixtures"]
fn reads_real_server_catalog_and_declarations() {
    let url = env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated Server URL required");
    let parsed = url::Url::parse(&url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
        "real Server test requires a loopback Dev/Test endpoint"
    );
    let app_id = env::var("APP_DEPOT_TEST_APP_ID").expect("fixture App ID required");
    let release_id = env::var("APP_DEPOT_TEST_RELEASE_ID").expect("fixture Release ID required");
    let token = env::var("APP_DEPOT_TEST_TOKEN").ok();
    let client = ApiClient::new(&url, token, false).unwrap();
    let caps = client.ensure_read_compatible().unwrap();
    assert!(
        caps.incompatibilities().is_empty(),
        "{:?}",
        caps.incompatibilities()
    );
    let apps = client.list_apps(caps.page_size().unwrap()).unwrap();
    assert!(apps.iter().any(|app| app.id == app_id));
    assert_eq!(client.app(&app_id).unwrap().id, app_id);
    let releases = client
        .list_releases(&app_id, caps.page_size().unwrap())
        .unwrap();
    assert!(releases.iter().any(|release| release.id == release_id));
    assert_eq!(client.release(&release_id).unwrap().id, release_id);
    let artifacts = client
        .associated_artifacts(&release_id, caps.page_size().unwrap())
        .unwrap();
    let expected = env::var("APP_DEPOT_TEST_ASSOCIATION_COUNT")
        .unwrap_or_else(|_| "1".into())
        .parse::<usize>()
        .unwrap();
    assert_eq!(artifacts.len(), expected);
    let snapshot = client.declarations(&release_id).unwrap();
    assert_eq!(snapshot.release_id, release_id);
    assert_eq!(snapshot.associations.len(), expected);
    assert_eq!(
        snapshot.release.data["unknown"]["large"].as_u64(),
        Some(u64::MAX)
    );

    let root = tempfile::tempdir().unwrap();
    let mut library = Library::open(LibraryPaths {
        metadata: root.path().join("metadata"),
        content: root.path().join("content"),
    })
    .unwrap();
    let profile = library.add_profile("isolated test", &url, false).unwrap();
    let summary = library.sync(&profile.id, &client).unwrap();
    assert!(summary.apps >= 1 && summary.releases >= 1);
    assert!(
        library
            .cached_apps(&profile.id)
            .unwrap()
            .iter()
            .any(|row| row.value.id == app_id)
    );
    let cached = library
        .cached_snapshot(&profile.id, &release_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.value.release.data["unknown"]["large"].as_u64(),
        Some(u64::MAX)
    );
    drop(library);
    let library = Library::open(LibraryPaths {
        metadata: root.path().join("metadata"),
        content: root.path().join("content"),
    })
    .unwrap();
    assert!(
        library
            .cached_snapshot(&profile.id, &release_id)
            .unwrap()
            .is_some()
    );
}

#[test]
#[ignore = "requires isolated loopback Dev/Test Server with a published File Artifact"]
fn downloads_and_resumes_real_file_artifact() {
    let url = env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated Server URL required");
    let parsed = url::Url::parse(&url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
        "real download test requires loopback Dev/Test Server"
    );
    let release_id = env::var("APP_DEPOT_TEST_RELEASE_ID").expect("fixture Release ID required");
    let client = ApiClient::new(&url, env::var("APP_DEPOT_TEST_TOKEN").ok(), false).unwrap();
    let caps = client.capabilities().unwrap();
    let association = client
        .associated_artifacts(&release_id, caps.page_size().unwrap())
        .unwrap()
        .into_iter()
        .find(|item| item.artifact.kind == "File" && item.artifact.size.unwrap_or(0) > 65536)
        .expect("published fixture needs File Artifact larger than 64 KiB to test Range resume");
    let root = tempfile::tempdir().unwrap();
    let paths = LibraryPaths {
        metadata: root.path().join("data"),
        content: root.path().join("content"),
    };
    let mut library = Library::open(paths.clone()).unwrap();
    let profile = library
        .add_profile("disposable download fixture", &url, false)
        .unwrap();
    let job = library
        .queue_file(
            &client,
            &profile.id,
            &release_id,
            &association.association.id,
        )
        .unwrap();
    assert_eq!(
        library
            .run_file_job(&client, &job.id, Some(1))
            .unwrap()
            .state,
        "Paused"
    );
    assert!(library.job(&job.id).unwrap().progress > 0);
    assert!(library.job(&job.id).unwrap().progress < job.expected_size);
    let paused = library.job(&job.id).unwrap();
    let mut request = ureq::get(&format!(
        "{}/api/v1/artifacts/{}/content",
        url.trim_end_matches('/'),
        job.artifact_id
    ))
    .set("Range", &format!("bytes={}-", paused.progress))
    .set("If-Range", paused.etag.as_deref().unwrap());
    if let Ok(token) = env::var("APP_DEPOT_TEST_TOKEN") {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = request.call().unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.header("Content-Range"),
        Some(
            format!(
                "bytes {}-{}/{}",
                paused.progress,
                job.expected_size - 1,
                job.expected_size
            )
            .as_str()
        )
    );
    assert_eq!(response.header("ETag"), paused.etag.as_deref());
    drop(library);
    let mut library = Library::open(paths).unwrap();
    library.retry_job(&job.id).unwrap();
    assert_eq!(
        library.run_file_job(&client, &job.id, None).unwrap().state,
        "Completed"
    );
    let file = library
        .verified_files(Some(&profile.id))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(file.artifact_id, job.artifact_id);
    assert_eq!(file.size, job.expected_size);
    assert!(file.path.exists());
}

#[test]
#[ignore = "requires isolated loopback Dev/Test Server with a Published multi-file Tree Artifact"]
fn downloads_and_resumes_real_tree_artifact() {
    let url = env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated Server URL required");
    let parsed = url::Url::parse(&url).unwrap();
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "::1")
    ));
    let release_id = env::var("APP_DEPOT_TEST_RELEASE_ID").expect("fixture Release ID required");
    let client = ApiClient::new(&url, env::var("APP_DEPOT_TEST_TOKEN").ok(), false).unwrap();
    let caps = client.capabilities().unwrap();
    let association = client
        .associated_artifacts(&release_id, caps.page_size().unwrap())
        .unwrap()
        .into_iter()
        .find(|item| item.artifact.kind == "Tree")
        .expect("Published Tree fixture required");
    let (entries, etag) = client.tree_inventory(&association.artifact.id, 1).unwrap();
    assert!(etag.starts_with('"'));
    assert_eq!(
        app_depot_core::tree::identity(&entries).unwrap(),
        association.artifact.id
    );
    assert!(entries.iter().any(|e| e.path() == "empty/nested"));
    let root = tempfile::tempdir().unwrap();
    let paths = LibraryPaths {
        metadata: root.path().join("data"),
        content: root.path().join("content"),
    };
    let mut library = Library::open(paths.clone()).unwrap();
    let profile = library
        .add_profile("disposable Tree fixture", &url, false)
        .unwrap();
    let job = library
        .queue_tree(
            &client,
            &profile.id,
            &release_id,
            &association.association.id,
        )
        .unwrap();
    assert_eq!(
        library
            .run_tree_job(&client, &job.id, Some(65536))
            .unwrap()
            .state,
        "Paused"
    );
    let files = library.tree_files(&job.id).unwrap();
    assert!(files.iter().any(|f| f.verified));
    let incomplete = files
        .iter()
        .find(|f| f.progress > 0 && !f.verified)
        .unwrap();
    let mut request = ureq::get(&format!(
        "{}/api/v1/artifacts/{}/files/{}",
        url.trim_end_matches('/'),
        job.artifact_id,
        incomplete.file_id
    ))
    .set("Range", &format!("bytes={}-", incomplete.progress))
    .set("If-Range", incomplete.etag.as_deref().unwrap());
    if let Ok(token) = env::var("APP_DEPOT_TEST_TOKEN") {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = request.call().unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        response.header("Content-Range"),
        Some(
            format!(
                "bytes {}-{}/{}",
                incomplete.progress,
                incomplete.size - 1,
                incomplete.size
            )
            .as_str()
        )
    );
    drop(library);
    let mut library = Library::open(paths.clone()).unwrap();
    library.retry_job(&job.id).unwrap();
    let paused = library.job(&job.id).unwrap();
    let mut observations = Vec::new();
    assert_eq!(
        library
            .run_tree_job_observed(&client, &job.id, None, &mut |event| observations
                .push(event))
            .unwrap()
            .state,
        "Completed"
    );
    assert_eq!(
        observations.iter().map(|event| event.network_bytes).max(),
        Some(paused.expected_size - paused.progress)
    );
    assert!(
        observations.iter().any(
            |event| event.phase == app_depot_core::transfer::TransferPhase::PreparingLocalFiles
        )
    );
    #[cfg(windows)]
    let installed = {
        let installation = library.install_windows_artifact(&job.id).unwrap();
        assert_eq!(installation.state, "Ready");
        assert_eq!(
            library.install_windows_artifact(&job.id).unwrap().id,
            installation.id
        );
        assert!(installation.path.join("payload/empty/nested").is_dir());
        installation
    };
    drop(library);
    let library = Library::open(paths).unwrap();
    let tree = library
        .verified_trees(Some(&profile.id))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(tree.artifact_id, association.artifact.id);
    assert!(tree.path.join("empty/nested").is_dir());
    assert!(tree.path.join("data/large.bin").is_file());
    #[cfg(windows)]
    {
        library.verify_windows_installation(&installed.id).unwrap();
        let changed = installed.path.join("payload/data/large.bin");
        let original = std::fs::read(&changed).unwrap();
        std::fs::write(&changed, vec![0; original.len()]).unwrap();
        assert!(library.verify_windows_installation(&installed.id).is_err());
        assert_eq!(
            std::fs::read(tree.path.join("data/large.bin")).unwrap(),
            original
        );
    }
    #[cfg(windows)]
    if let Ok(corrupt_url) = env::var("APP_DEPOT_TEST_CORRUPT_TREE_URL") {
        assert!(matches!(
            url::Url::parse(&corrupt_url).unwrap().host_str(),
            Some("127.0.0.1" | "::1")
        ));
        let corrupt = ApiClient::new(&corrupt_url, None, false).unwrap();
        let isolated = tempfile::tempdir().unwrap();
        let mut library = Library::open(LibraryPaths {
            metadata: isolated.path().join("metadata"),
            content: isolated.path().join("content"),
        })
        .unwrap();
        let profile = library
            .add_profile("corrupt fixture", &corrupt_url, false)
            .unwrap();
        let job = library
            .queue_tree(
                &corrupt,
                &profile.id,
                &release_id,
                &association.association.id,
            )
            .unwrap();
        assert!(
            library
                .run_tree_job(&corrupt, &job.id, None)
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
        assert_eq!(library.job(&job.id).unwrap().state, "Failed");
        assert!(library.verified_trees(None).unwrap().is_empty());
        assert!(library.install_windows_artifact(&job.id).is_err());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(tree.path.join("bin/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires isolated loopback Dev/Test Server with a prerequisite-free published Standard manifest"]
fn installs_real_standard_plan_and_reuses_downloads() {
    use app_depot_core::installation::ResolveRequest;
    let url = env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated Server URL required");
    assert!(matches!(
        url::Url::parse(&url).unwrap().host_str(),
        Some("127.0.0.1" | "localhost" | "::1")
    ));
    let release = env::var("APP_DEPOT_TEST_RELEASE_ID").unwrap();
    let client = ApiClient::new(&url, env::var("APP_DEPOT_TEST_TOKEN").ok(), false).unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut library = Library::open(LibraryPaths {
        metadata: root.path().join("metadata"),
        content: root.path().join("content"),
    })
    .unwrap();
    let profile = library
        .add_profile("synthetic Standard fixture", &url, false)
        .unwrap()
        .id;
    let saved = library
        .resolve_plan(
            &profile,
            &client,
            &release,
            &ResolveRequest {
                platform: format!("linux/{}", std::env::consts::ARCH),
                revision_id: None,
                selections: Default::default(),
            },
        )
        .unwrap();
    let installation = library
        .install_saved_plan(&profile, &saved.id, Some(&client))
        .unwrap();
    assert_eq!(installation.generations[0].state, "Ready");
    library
        .verify_release_installation(&profile, &release)
        .unwrap();
    let candidates = library.launch_candidates(&profile, &release).unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate.status, "Ready");
    assert!(
        library
            .play_declared(
                &profile,
                &release,
                &candidate.association_id,
                &candidate.entry_id,
                candidate.selection_revision()
            )
            .unwrap()
            .wait()
            .unwrap()
            .success()
    );
    let jobs = library.jobs(Some(&profile)).unwrap();
    assert!(jobs.len() >= 2 && jobs.iter().all(|j| j.state == "Completed"));
    library
        .uninstall_release_installation(&profile, &release)
        .unwrap();
    library
        .install_saved_plan(&profile, &saved.id, None)
        .unwrap();
    library
        .verify_release_installation(&profile, &release)
        .unwrap();
    assert_eq!(
        library
            .jobs(Some(&profile))
            .unwrap()
            .iter()
            .map(|j| &j.id)
            .collect::<Vec<_>>(),
        jobs.iter().map(|j| &j.id).collect::<Vec<_>>()
    );
}
