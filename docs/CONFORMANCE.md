# Conformance and integration plan

Use the Server OpenAPI as the route/schema source and `docs/tree-identity.md` vectors as identity fixtures. Pin the tested Server commit and record the capabilities response in test output. Tests run against a separately initialized Dev/Test Server data root and a temporary client library, with disposable users/tokens and deterministic fixtures. Never connect integration tests to Production or reset an existing user library. The client has no privileged test-only bypass.

## Contract tests

| Area | Required cases and expected behavior |
| --- | --- |
| Catalog/auth | Anonymous enabled/disabled, Public Published read, Bearer owner Private/Draft read, inaccessible Private 404, missing token 401, forbidden visible action 403. Paginate Apps, Releases, associations and revisions; invalid cursor `400 invalid_cursor`; query/scope changes invalidate cursor. |
| Capabilities | Supported identity formats/ranges; missing or unknown fields, different API version, advertised lower page limit and incompatible identity format yield explicit partial/unsupported state, never silent fallback. |
| Declarations | Missing, null, scalar, unknown fields and incomplete entry points; raw JSON survives typed projection. Multiple associations and points remain distinct. Validate declared target: valid, missing, not-file, invalid and unsupported. No entry point remains downloadable without Launch. |
| Discovery/selection | Optional channels, no channel, multiple Releases, multiple Artifacts per Release; no implicit primary or guessed latest. Platform absent is visible; unsupported platform/runtime and unsupported installation method are explained without execution. |
| Artwork | Missing role 404 gives placeholder; valid image displays, wrong/oversized/unsafe content is rejected locally; private artwork obeys authorization. |
| File identity | Download a file including zero-byte file, verify exact length and SHA-256 against `adfile:sha256`. Hash mismatch never reaches Completed. |
| Tree identity | Fetch all unfiltered inventory pages; verify paths, fileId opacity, sizes, hashes, executable flags, explicit empty dirs and v1 vectors. Reject traversal, absolute paths, backslashes, duplicate/case/Unicode collisions on target filesystem, symlink destinations and special/unrepresentable names. Do not repair path spelling. |
| Resume and HTTP | Interrupt mid-file and mid-tree, restart process, resume validated bytes. Matching `If-Range` 206 with exact `Content-Range`; mismatching validator/full 200 restarts file; 416 revalidates; truncated body or wrong range fails. Multiple-range full 200 is accepted only as a full restart. Bounded retries for transport/429/503; clear terminal errors for 400/401/403/404 and digest mismatch. |
| Local persistence | Completed files offline, incomplete jobs recover, committing-state crash recovery, no false Completed after interrupted fsync/rename, two profiles with same remote IDs stay separate, shared bytes retain independent contexts, local removal does not delete another reference. |
| Launch safety | Artifact with valid declared point is still inert in M1; unsupported runtime/procedure and unknown extension are visible. Future implementation tests reject traversal targets, unsafe working directories, argument interpolation/shell metacharacter execution and undeclared operations. |

## Test layers

1. Pure unit tests for identity/path validation, JSON preservation, compatibility assessment and transfer state machine, using Server Tree Identity v1 positive and negative vectors.
2. HTTP contract tests driven by OpenAPI shapes and isolated Server fixtures, including error bodies, ETags, exact integer parsing, pagination and Range semantics.
3. Process-level interruption tests with real temporary library and real Dev Server: kill client during transfer/commit, restart and verify recovery. The Server's existing `crates/server/tests/upload_http.rs` is evidence for Server behavior, not a substitute for client download tests.
4. Linux GUI smoke tests for catalog, details, queue and offline library. Windows/macOS filesystem and secure-store checks run when those platform milestones start.

No test should execute downloaded software or modify Production Server state. A fake HTTP server is appropriate for malformed responses and adversarial paths; real Server tests remain required for the actual protocol.

## M1.1 implemented coverage

The read-only core tests cover capabilities and unknown fields, exact large integers, Bearer versus anonymous requests, RFC 9457 errors, expired/repeated cursors, multi-page traversal, multiple associations, safe URL encoding, refused redirects and malformed/oversized responses. `crates/app-depot-core/tests/real_server.rs` is deliberately ignored in routine runs and requires an isolated published fixture when invoked with `--ignored`; it reads capabilities, catalog, Artifact metadata and the consistent declaration snapshot without mutating the Server. Transfer, artwork, GUI and launch tests remain for their later milestones.

## M1.2 implemented coverage

The local-library tests create/reopen SQLite schema 1, reject unknown schemas and unsafe roots, distinguish profiles with equal remote IDs, archive old identities on URL change/removal, reject unavailable native storage without plaintext fallback, preserve full JSON integers, fetch a mock catalog and commit it atomically, roll back on a database error, keep prior cache after a failed paginated fetch, mark unseen records stale after a complete later sync, and read cached metadata offline. The ignored real-Server test additionally syncs an isolated Published fixture into a temporary client library, reopens it, and checks the declaration value `u64::MAX`. Linux Secret Service was exercised with a disposable token and the token was removed. Transfer, artwork, GUI, and Windows/macOS secure-store/filesystem tests remain for later milestones.
