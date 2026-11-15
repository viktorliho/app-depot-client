# App Depot Client

Native reference client for the open [App Depot Server](https://github.com/viktorliho/app-depot-server) protocol. The client is optional: independent clients can implement the documented Server API without using this code.

M1.1 provides a read-only HTTP core and diagnostic CLI. There is no desktop UI, downloader, installer or launcher yet. See [milestones](docs/MILESTONES.md) and the [server API audit](docs/SERVER_API_AUDIT.md).

## Build and use

```sh
cargo build --workspace --locked
cargo run -p app-depot -- --server http://127.0.0.1:8787 capabilities
cargo run -p app-depot -- --server http://127.0.0.1:8787 apps
cargo run -p app-depot -- --server http://127.0.0.1:8787 releases APP_ID
cargo run -p app-depot -- --server http://127.0.0.1:8787 artifacts RELEASE_ID
cargo run -p app-depot -- --server http://127.0.0.1:8787 declarations RELEASE_ID --json
```

Also available: `app APP_ID` and `release RELEASE_ID`. `--json` produces machine-readable JSON with exact integer values. Set `APP_DEPOT_TOKEN` for a temporary Bearer token; it is never stored. `APP_DEPOT_SERVER_URL` can provide the URL instead of `--server`. Remote HTTP requires `--allow-insecure-http`; HTTPS does not. The client refuses redirects. Exit codes: 2 configuration, 3 authentication, 4 authorization, 5 transport, 6 incompatible capabilities, 7 malformed protocol response, 8 other HTTP error.

This release implements the Server's `apiVersion: 1.0.0` read contract. Other API versions are reported as unreviewed until their contract is checked. A missing download Range capability or unknown identity format is reported by `capabilities`; read-only catalog commands can still operate when the read contract is compatible.

The real Server test is ignored in routine runs. Against an isolated Dev/Test Server with a Published fixture containing an unknown `uint64` field, set `APP_DEPOT_TEST_SERVER_URL`, `APP_DEPOT_TEST_APP_ID`, `APP_DEPOT_TEST_RELEASE_ID`, and optionally `APP_DEPOT_TEST_TOKEN` and `APP_DEPOT_TEST_ASSOCIATION_COUNT`, then run `cargo test -p app-depot-core --test real_server --locked -- --ignored`. Never point it at Production.

The client treats Server declarations and downloaded content as untrusted. A download never implies permission or ability to install or execute it.
