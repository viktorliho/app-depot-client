# App Depot Client

Native reference client for the open [App Depot Server](https://github.com/viktorliho/app-depot-server) protocol. The client is optional: independent clients can implement the documented Server API without using this code.

M1.5 adds a native egui desktop browser using the same HTTP core, profiles, offline cache and verified File/Tree download engine as the CLI. It does not install or launch software. See [milestones](docs/MILESTONES.md) and the [server API audit](docs/SERVER_API_AUDIT.md).

## Build and use

```sh
cargo build --workspace --locked
cargo run -p app-depot-desktop --locked
cargo run -p app-depot -- --server http://127.0.0.1:8787 capabilities
cargo run -p app-depot -- --server http://127.0.0.1:8787 apps
cargo run -p app-depot -- --server http://127.0.0.1:8787 releases APP_ID
cargo run -p app-depot -- --server http://127.0.0.1:8787 artifacts RELEASE_ID
cargo run -p app-depot -- --server http://127.0.0.1:8787 declarations RELEASE_ID --json
```

The desktop app has Catalog, Library, Downloads and Settings views. Add a profile in Settings, select it, then synchronize while online. Catalog offers grid/table views, search, App details, all Release associations and individual File/Tree download actions. Library and Downloads read the same durable local state as the CLI; completed downloads remain visible after restart or when the Server is unavailable. Artwork is loaded asynchronously from the Server's App artwork API and cached in memory for the session; missing or rejected images use a placeholder. The UI never presents Install or Launch actions. `APP_DEPOT_LIBRARY_DATA_DIR` and `APP_DEPOT_LIBRARY_CONTENT_DIR` choose disposable or separate roots for the desktop app, using the same validation as the CLI. On Linux, Wayland and X11 are supported. The GUI requires Rust 1.95 or newer.

Also available: `app APP_ID` and `release RELEASE_ID`. `--json` produces machine-readable JSON with exact integer values. Set `APP_DEPOT_TOKEN` for a temporary Bearer token; it is never stored. `APP_DEPOT_SERVER_URL` can provide the URL instead of `--server`. Remote HTTP requires `--allow-insecure-http`; HTTPS does not. The client refuses redirects. Exit codes: 2 configuration, 3 authentication, 4 authorization, 5 transport, 6 incompatible capabilities, 7 malformed protocol response, 8 other HTTP error.

## Profiles and offline catalog

```sh
app-depot profile add "Home Depot" https://depot.example.org
app-depot profile list
app-depot --profile PROFILE_ID capabilities
app-depot --profile PROFILE_ID sync
app-depot --profile PROFILE_ID cached-apps
app-depot --profile PROFILE_ID cached-releases APP_ID
app-depot --profile PROFILE_ID cached-declarations RELEASE_ID --json
app-depot profile rename PROFILE_ID "New name"
app-depot profile move PROFILE_ID https://new-depot.example.org
app-depot profile remove PROFILE_ID --confirm
```

`profile move` creates a new profile ID and archives the old profile and its cache; remove a stored token first with `profile token-remove PROFILE_ID`. `profile remove --confirm` archives the profile, removes its stored token, and retains cached metadata for offline reading through `--profile PROFILE_ID`; `profile list --all` shows archived profiles. There is no cache purge command in M1.2. Profiles with the same URL remain separate. `sync` retrieves all accessible Apps, Releases, associations, Artifact metadata and declaration snapshots before writing a SQLite transaction. A failed sync keeps the previous cache. A successful sync marks unseen older records stale but retains them. Cached output always says `source: cache` in JSON and includes last-sync and stale fields; it does not claim a globally atomic Server catalog snapshot.

To store a profile token, pipe it to `app-depot profile token-set PROFILE_ID --token-stdin`; replacement updates that profile's existing secure-store entry. On Linux the native backend is Secret Service, with Credential Manager and Keychain selected on Windows and macOS. If unavailable, use anonymous access or `APP_DEPOT_TOKEN` for one process; there is no plaintext fallback. `--library-data ABSOLUTE_PATH` or `APP_DEPOT_LIBRARY_DATA_DIR` selects the metadata root. `--library-content ABSOLUTE_PATH` or `APP_DEPOT_LIBRARY_CONTENT_DIR` selects the content root separately. With only `--library-data`, both roots use that path. Defaults are the OS user data directory under `app-depot-client`. Local-library failures exit 9 and native credential failures exit 10.

## Artifact downloads

```sh
app-depot --profile PROFILE_ID download start RELEASE_ID ASSOCIATION_ID
app-depot --profile PROFILE_ID download jobs
app-depot --profile PROFILE_ID download pause JOB_ID
app-depot --profile PROFILE_ID download resume JOB_ID
app-depot --profile PROFILE_ID download retry JOB_ID
app-depot --profile PROFILE_ID download cancel JOB_ID
app-depot --profile PROFILE_ID download files
app-depot --profile PROFILE_ID download trees
app-depot --profile PROFILE_ID download tree-files JOB_ID
```

`download start` requires an accessible Release association and creates a durable File or Tree job; use `artifacts RELEASE_ID` to find its association ID. The command prints the job ID before transfer. `--pause-after-bytes N` pauses after at least N accepted bytes for diagnostics. A process interruption leaves a resumable job, and `download jobs` reconciles interrupted states. `download files` and `download trees` validate and list verified local Artifacts offline, including absolute materialized paths. `download tree-files JOB_ID` shows each member's path, progress and verification state. `--json` applies to download commands. A cancelled job retains verified member objects and staging data but cannot be resumed; no completed bytes are removed. An archived profile can list jobs and verified Artifacts offline but cannot download. Tree paths and executable flags come only from verified Server inventory; no entry point, installation or launch behavior is inferred.

The client implements the Server's `apiVersion: 1.0.0` read, File and Tree transfer contract. Other API versions are reported as unreviewed until their contract is checked. Tree transfer requires `adtree:v1:sha256`, `adfile:sha256` and advertised Range support. A missing download Range capability or unknown identity format is reported by `capabilities`; read-only catalog commands can still operate when the read contract is compatible. Linux Tree completion uses atomic no-replace directory promotion. Windows/macOS Tree completion still needs an equivalent implementation and validation.

The real Server tests are ignored in routine runs. Against an isolated Dev/Test Server with a Published fixture containing an unknown `uint64` field, a File Artifact larger than 64 KiB, and a Tree Artifact with multiple files, nested and empty directories, executable flag and a large member, set `APP_DEPOT_TEST_SERVER_URL`, `APP_DEPOT_TEST_APP_ID`, `APP_DEPOT_TEST_RELEASE_ID`, and optionally `APP_DEPOT_TEST_TOKEN` and `APP_DEPOT_TEST_ASSOCIATION_COUNT`, then run `cargo test -p app-depot-core --test real_server --locked -- --ignored`. For the Linux process interruption test, also set `APP_DEPOT_TEST_TREE_ASSOCIATION_ID` and run `cargo test -p app-depot --test tree_sigkill --locked -- --ignored`. These tests use disposable local libraries and exercise inventory pagination, interruption, Range resume, SHA-256, Tree Identity v1 and offline materialization. Never point them at Production.

The client treats Server declarations and downloaded content as untrusted. A download never implies permission or ability to install or execute it.
