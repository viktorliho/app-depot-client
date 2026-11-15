//! Read-only compatibility check against an isolated Dev/Test Server.

use app_depot_core::api::ApiClient;
use std::env;

#[test]
#[ignore = "requires isolated Dev/Test Server and published App/Release fixtures"]
fn reads_real_server_catalog_and_declarations() {
    let url = env::var("APP_DEPOT_TEST_SERVER_URL").expect("isolated Server URL required");
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
}
