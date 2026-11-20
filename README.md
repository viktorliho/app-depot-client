# App Depot Client

Native reference client for the open [App Depot Server](https://github.com/viktorliho/app-depot-server) protocol. The client is optional: independent clients can implement the documented Server API without using this code.

The native egui desktop launcher shares the CLI's HTTP core, profiles, offline cache, verified File/Tree downloads and Linux NativePortableV1 launch operation. Native portable programs use a separate writable local working copy; download-only Artifacts remain valid. See [milestones](docs/MILESTONES.md) and the [server API audit](docs/SERVER_API_AUDIT.md).

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

On first launch, choose **Add Server**, enter its URL and connect. The desktop checks capabilities and catalog access before saving a profile. If the catalog requires a personal API token, use **Open Server Account page** to create one, paste it, then choose secure native storage or a session-only token. Explicit LAN HTTP requires its warning checkbox. The catalog loads automatically after connection. Existing profiles reopen from cache immediately and refresh in the background when stale; routine refresh is at most every 10 minutes per active profile, with bounded backoff after transient failures and no automatic retry loop for rejected credentials. **Refresh** remains available in Browse. Switching servers changes the visible catalog immediately. Offline metadata stays visible, and verified local files and trees remain available in **My library**.

Browse offers responsive grid/list views, search (`Ctrl+F`), App details, all Releases and every Artifact association. Download actions apply to the selected association only. A completed catalog refresh hides remotely absent Apps from connected Browse; their cached metadata and verified local downloads remain available offline and in My library. My library shows each verified download with saved App and Release context and opens its containing folder. A verified, compatible NativePortableV1 entry point is prepared automatically after download and shows Play on Linux; older downloads prepare transparently on first Play. Multiple candidates have separate explicit actions. The worker checks the local target and displayed declaration revision again before starting a direct process and reports Starting, Running, Exited or Launch failed. Downloads uses one prioritized library-wide queue across profiles. A new request takes priority, and the previous transfer yields at a durable checkpoint and resumes automatically; user-paused jobs remain paused. The view shows durable jobs, live received bytes, measured transfer speed, an ETA when enough data exists, Tree file counts and available pause/resume/retry/cancel/move-to-top actions. Technical IDs and exact raw declarations remain available in Advanced sections. Artwork is loaded asynchronously from bounded Server artwork routes. Server-declared installation procedures remain unsupported. `APP_DEPOT_LIBRARY_DATA_DIR` and `APP_DEPOT_LIBRARY_CONTENT_DIR` choose disposable or separate roots for the desktop app. Appearance preferences, including a bounded UI scale, stay in `desktop-preferences.json` without credentials. Linux uses a custom draggable titlebar and resize edges; Windows/macOS keep native window decorations until validated. Linux Wayland and X11 have been exercised; native Wayland clipboard paste in this revision has not been manually revalidated. Windows/macOS native launch is not implemented. The GUI requires Rust 1.95 or newer.

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

To store a profile token, pipe it to `app-depot profile token-set PROFILE_ID --token-stdin`; replacement updates that profile's existing secure-store entry. On Linux the native backend is Secret Service, with Credential Manager and Keychain selected on Windows and macOS. If unavailable, use anonymous access or `APP_DEPOT_TOKEN` for one process; there is no plaintext fallback. `--library-data ABSOLUTE_PATH` or `APP_DEPOT_LIBRARY_DATA_DIR` selects the metadata root. `--library-content ABSOLUTE_PATH` or `APP_DEPOT_LIBRARY_CONTENT_DIR` selects the content root separately. With only `--library-data`, both roots use that path. New Linux libraries default to `~/.appdepot`; marked older XDG libraries stay active until explicit migration. Windows and macOS retain their OS user-data defaults. Local-library failures exit 9 and native credential failures exit 10.

## Artifact downloads

```sh
app-depot --profile PROFILE_ID download start RELEASE_ID ASSOCIATION_ID
app-depot --profile PROFILE_ID download enqueue RELEASE_ID ASSOCIATION_ID
app-depot --profile PROFILE_ID download jobs
app-depot --profile PROFILE_ID download move-top JOB_ID
app-depot --profile PROFILE_ID download pause JOB_ID
app-depot --profile PROFILE_ID download resume JOB_ID
app-depot --profile PROFILE_ID download retry JOB_ID
app-depot --profile PROFILE_ID download cancel JOB_ID
app-depot --profile PROFILE_ID download files
app-depot --profile PROFILE_ID download trees
app-depot --profile PROFILE_ID download tree-files JOB_ID
```

`download start` requires an accessible Release association and creates a durable File or Tree job; use `artifacts RELEASE_ID` to find its association ID. The command prints the job ID before transfer. `download enqueue RELEASE_ID ASSOCIATION_ID` adds the association to the desktop-managed queue without starting a CLI transfer; `download move-top JOB_ID` raises a pending job's priority. The direct `start` command shares the same library-wide transfer slot and leaves its job queued if the desktop or another CLI process owns it. `--pause-after-bytes N` pauses after at least N accepted bytes for diagnostics. A process interruption leaves a resumable job, and `download jobs` reconciles interrupted states. `download files` and `download trees` validate and list verified local Artifacts offline, including absolute materialized paths. `download tree-files JOB_ID` shows each member's path, progress and verification state. `--json` applies to download commands. A cancelled job retains verified member objects and staging data but cannot be resumed; no completed bytes are removed. An archived profile can list jobs and verified Artifacts offline but cannot download. Tree paths and executable flags come only from verified Server inventory; no entry point, installation or launch behavior is inferred.

The client implements the Server's `apiVersion: 1.0.0` read, File and Tree transfer contract. Other API versions are reported as unreviewed until their contract is checked. Tree transfer requires `adtree:v1:sha256`, `adfile:sha256` and advertised Range support. A missing download Range capability or unknown identity format is reported by `capabilities`; read-only catalog commands can still operate when the read contract is compatible. Linux Tree completion uses atomic no-replace directory promotion. Windows/macOS Tree completion still needs an equivalent implementation and validation.

The real Server tests are ignored in routine runs. Against an isolated Dev/Test Server with a Published fixture containing an unknown `uint64` field, a File Artifact larger than 64 KiB, and a Tree Artifact with multiple files, nested and empty directories, executable flag and a large member, set `APP_DEPOT_TEST_SERVER_URL`, `APP_DEPOT_TEST_APP_ID`, `APP_DEPOT_TEST_RELEASE_ID`, and optionally `APP_DEPOT_TEST_TOKEN` and `APP_DEPOT_TEST_ASSOCIATION_COUNT`, then run `cargo test -p app-depot-core --test real_server --locked -- --ignored`. For the Linux process interruption test, also set `APP_DEPOT_TEST_TREE_ASSOCIATION_ID` and run `cargo test -p app-depot --test tree_sigkill --locked -- --ignored`. These tests use disposable local libraries and exercise inventory pagination, interruption, Range resume, SHA-256, Tree Identity v1 and offline materialization. Never point them at Production.

The client treats Server declarations and downloaded content as untrusted. A download never implies permission or ability to install or execute it.

## Native portable launch on Linux

After synchronizing a Server with the M2.0 declaration contract and downloading the selected File or Tree Artifact, inspect every cached launch choice offline. The desktop prepares a supported writable installation after download. A direct CLI download or an older verified download prepares transparently on first Play:

```sh
app-depot --profile PROFILE_ID launch list RELEASE_ID
app-depot --profile PROFILE_ID launch play RELEASE_ID ASSOCIATION_ID ENTRY_POINT_ID
```

`runtime prepare ASSOCIATION_ID` remains an advanced explicit command that uses verified local objects without contacting the Server. Linux tries reflinks and falls back to independent copies. The launch command is an explicit request to run the declared program and waits for its exit; a nonzero exit is reported as an error. It uses the cached Release declaration snapshot and a writable local installation, preparing one transparently if necessary, so an archived profile or unavailable Server does not prevent an otherwise safe offline launch. Unsupported, malformed and legacy target-only declarations remain inert; multiple eligible candidates require explicit selection. The desktop offers Play for a compatible verified download and never guesses a target. A direct Linux child process uses the declared Artifact-relative working directory and literal argv; no shell or Dolphin is involved. Play checks target type, executable mode and path confinement but does not hash installed files. User patches, mods and configuration changes are permitted. `runtime verify INSTALLATION_ID` explicitly reports missing, modified and unexpected files without changing them. Confirmed `runtime repair INSTALLATION_ID --confirm` keeps the old installation, including additions, in managed staging and restores original files from verified objects; managed user data stays. A prior repair backup must be inspected before a later repair can overwrite it. The client strips App Depot token and secret variables from the child's inherited environment. Closing the desktop does not intentionally stop a running child. See [the lifecycle contract](docs/DECLARATIVE_LIFECYCLE.md) for precise limits.


## Managed local library on Linux

The immutable verified download remains under `objects/` and `artifacts/`. Automatic preparation creates a separate writable copy under `apps/<local-id>/` and a managed `userdata/<local-id>/` directory. A game may change files in its working copy without invalidating the original Tree identity. App Depot does not sandbox native programs: files they create outside the managed library are not removed automatically.

```sh
app-depot library info
app-depot library usage
app-depot --profile PROFILE_ID runtime list
app-depot --profile PROFILE_ID runtime verify INSTALLATION_ID
app-depot --profile PROFILE_ID runtime preview INSTALLATION_ID
app-depot --profile PROFILE_ID runtime preview-complete INSTALLATION_ID
app-depot --profile PROFILE_ID runtime remove INSTALLATION_ID --confirm
app-depot --profile PROFILE_ID runtime remove-complete INSTALLATION_ID --confirm
app-depot --profile PROFILE_ID runtime remove-artifact ASSOCIATION_ID --confirm
app-depot --profile PROFILE_ID runtime remove-app APP_ID --confirm
app-depot --profile PROFILE_ID runtime remove-app-complete APP_ID --confirm
app-depot --profile PROFILE_ID runtime repair INSTALLATION_ID --confirm
app-depot --profile PROFILE_ID runtime repair-source ASSOCIATION_ID --confirm
app-depot library gc --confirm
```

The read-only previews list the selected local identity and logical sizes before deletion. Normal removal deletes writable copies and retains verified downloads and managed data. Complete removal deletes the selected local references and managed data, while preserving content shared by other Apps, Releases, Associations or profiles. It never deletes Server resources or the profile token. Repairing a damaged Tree source from verified local objects preserves the former directory, including extra files, in `staging/source-quarantine-<job-id>` until complete removal. `library usage` reports logical sizes; reflinks and compression mean physical space reclaimed may differ.

For an older marked XDG library, close the desktop and pause active downloads before an explicit copy to the new default. The source is preserved:

```sh
app-depot library info
app-depot library migration-preview "$HOME/.appdepot"
app-depot library migrate "$HOME/.appdepot" --confirm
```

If metadata and content use separate roots, pass their absolute paths through `--library-data` and `--library-content` before `library migrate`. Inspect the new library with `app-depot library info` and `app-depot --profile PROFILE_ID runtime list`; the old root is not removed automatically. A failed migration leaves the source usable and may leave a `.appdepot-migrating-<UUID>` directory for manual inspection. Do not delete an old root merely because its name resembles a test library.
