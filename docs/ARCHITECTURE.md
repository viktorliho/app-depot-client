# Client architecture

The Server stores authoritative declarations and immutable Artifact bytes. The client reads them, chooses only operations it understands, asks for appropriate user authority, and verifies every download. Local state records what this client has downloaded or performed; it never alters the Server's meaning. Independent clients can implement the same Server contract without this repository.

## Decision records

### ADR-001: Rust core, egui adapter

Use a native Rust core with an egui desktop UI on Linux first, then Windows/macOS. A CLI uses the same core for diagnostics and conformance. The current workspace implements the M1.1 read-only HTTP core and CLI, M1.2 profiles/catalog cache, M1.3 File transfer, M1.4 Tree transfer and the M1.5 desktop adapter. Future modules/crates are introduced when concrete later responsibilities exist:

| Boundary | Responsibility |
| --- | --- |
| Protocol transport | HTTP, Bearer header, pagination, exact JSON/uint64 handling, RFC 9457 errors, capability negotiation; no UI or filesystem policy. |
| Domain/interpreter | App, Release, Association, Artifact and declaration scope; raw JSON retained beside typed recognized views; explicit support assessment. No filename-based inference. |
| Transfer engine | Durable jobs, bounded concurrent file transfers, Range/If-Range, SHA-256, Tree Identity v1, path safety, atomic completion. |
| Local library | Profiles, snapshots, jobs, verified content and offline views; storage transactions and recovery. |
| Install/runtime/launch ports | Future capability-aware implementations. Each exposes supported declaration versions/methods and a preparation/launch plan; UI cannot call arbitrary commands directly. No such implementation in M0/M1. |
| GUI/CLI | Commands and read models from core. GUI renders grid/table, details and queue; CLI exposes capability checks, catalog, validation and download diagnostics. |

The core exposes synchronous operations and durable state snapshots. The desktop crate sends commands to a worker that performs HTTP, SQLite and hashing off the UI thread; separate transfer threads use the existing core jobs, and the UI receives events and refreshed snapshots. Artwork has a separate bounded fetch/decode worker so a slow image cannot stall catalog operations. The CLI is a first-class consumer, so another UI would not require splitting logic out of egui. The protocol itself remains language neutral.

### ADR-002: Explicit identity and selection

Generate a stable local `ServerProfileId` on profile creation; key all Server-local records by it and the Server ID. URL is editable and never used as identity. Keep separate App, Release, Association, Artifact and declaration revision IDs. A shared Artifact digest may reuse verified bytes internally, but access and display context remain profile/association scoped. Do not merge two Server profiles because their digests match.

Show every eligible Release and every association. Optional channels are labels/pointers; no implicit `stable` or `latest`. A Release version is display text, not a total ordering key. Platform-free Artifacts remain visible and downloadable. Unknown platform tokens are visible as unassessed; only recognized, explicitly mapped tokens can produce a compatibility verdict. No association is “primary” unless a future shared declaration explicitly establishes that concept.

### ADR-003: Download is independent of installation and launch

Model `Download`, `Install`, `RuntimePreparation`, `Launch` as distinct requests and records. `DownloadedVerified` means content integrity only. A declared entry point has a target, but current Server conventions do not specify execution mechanism, arguments or runtime preparation. It does not make a Launch action functional. Keep all unsupported declarations visible with an explanation. Treat declarations as metadata, not security authorization: even future supported operations require user consent and local security checks.

### ADR-004: Capability-aware compatibility

At connection, read `/capabilities`; retain unknown fields, check the API major/version policy and identity formats actually required for requested operations, and check `downloadRanges`. Never infer support from the Server product version alone. If a capability is absent or incompatible, show catalog where safely possible and disable only affected operations. Do not treat current `apiVersion: 1.0.0` as a stable product 1.0 guarantee. Future declaration operations need their own versioned, shared vocabulary; unknown versions remain opaque.

### ADR-005: Credentials

Use the OS secure store (Secret Service on Linux, Credential Manager on Windows, Keychain on macOS) for Bearer tokens, with an opaque credential reference attached to one local profile ID. Keep profile metadata and token references in the library database; never serialize tokens into jobs, logs, crash reports or exports. If secure storage is unavailable, offer only session-memory tokens or anonymous access. An explicit user opt-in may use a passphrase-encrypted local vault in a later milestone after its format and recovery policy are specified; plaintext fallback is prohibited. Use HTTPS for remote servers by default; allow loopback HTTP for local development and explicit user override for self-hosted LAN endpoints with a clear transport warning. Do not send credentials across a redirect to another origin.

### ADR-006: Local identity, cache and profile removal (M1.2)

SQLite schema version 1 stores each profile under a generated UUID and scopes Apps, Releases, associations, Artifacts and declaration snapshots by that ID. A profile URL change creates a new profile ID and archives the old one; this keeps old cached declarations attached to their original origin. Profile removal requires `--confirm`, removes its secure token, and archives the profile and cache. Archived profiles remain readable offline but cannot synchronize. No M1.2 command deletes cached metadata or verified content. A successful sync fetches the reachable catalog first, then writes it in one transaction. Unseen records are retained and marked stale; a failed fetch does not partially replace the cache. The Server provides a consistent declaration snapshot per Release, not a globally atomic catalog snapshot.

### ADR-007: File transfer and cross-filesystem completion (M1.3)

Use one staging file and a local advisory job lock for each File Artifact transfer. Store a synchronized byte checkpoint and SHA-256 prefix digest in SQLite, then rehash that prefix on resume before issuing `Range` with a strong `If-Range` validator. A matching `206` must identify the exact remainder; a `200` restarts from zero. Keep objects and user-visible materializations under the same content root so hard links provide atomic no-replace promotion without another full copy. Synchronize file and content directories before committing `Completed` in SQLite; reconcile an interrupted `Committing` state by re-verifying the bytes. Metadata and content roots may be on separate filesystems, so no cross-filesystem transaction is assumed. Reuse verified object bytes across profiles while retaining separate profile/association records. This is a local storage decision, not a Server protocol field.

### ADR-008: Tree inventory and atomic materialization (M1.4)

Fetch the entire unfiltered inventory with the Server's page limit and opaque cursors. Require one strong ETag, canonical order, valid file IDs and exact Tree Identity v1 before writing destination paths. Persist the complete inventory and per-member checkpoints in schema version 3. A member file uses the same Range/If-Range/SHA-256 receiver as a standalone File Artifact but becomes only an immutable local object; it is never registered as a separate Association. Construct the visible tree from verified objects in a private staging directory on the content filesystem, preserving explicit empty directories and only the portable executable bit. On Unix, create paths relative to directory descriptors with `openat`/`mkdirat` and `O_NOFOLLOW`. On Linux, `renameat2(RENAME_NOREPLACE)` promotes a verified tree without replacing another destination. Synchronize files and directories before the SQLite Completed transaction; reconcile Committing after a crash by re-verifying the destination or rebuilding from verified objects. Other platforms need an equivalent no-replace directory promotion before Tree completion is enabled. Tree identity is verified independently of destination naming compatibility.

### ADR-009: Desktop command and snapshot adapter (M1.5)

Keep the desktop application in its own workspace crate. UI commands select a profile, synchronize, manage credentials, start or control a specific association job, or reveal a verified local path. Worker events provide immutable views of cached Apps, Releases, associations, jobs and verified content. The worker owns library access; transfer threads open their own library handles and use the existing File/Tree core operations. The UI polls durable progress and never infers completion from network callbacks. Artwork requests use documented App artwork routes, capped response bytes and decoded dimensions, and a profile/App/role/binding-keyed in-memory texture cache. Binding changes invalidate textures on synchronization. Missing artwork is optional. This adapter adds no Server protocol fields and no installation or launch authority.

### ADR-010: Offline-first desktop presentation and bounded refresh (M1.5.1)

Keep presentation choices in a small owner-only JSON file beside the local database: system/light/dark theme, grid/list view and last profile ID. Do not store tokens there. First-run connection performs a background capability and catalog-access check before creating a profile. A private catalog asks for an existing personal API token; the Server Account page is opened through a validated origin. The token is verified before being placed in native secure storage or session memory. A secure-store failure leaves session-only access available without a plaintext fallback.

Selecting a profile reads its cached catalog immediately. The worker refreshes the active profile after connection or credential change, when stale at startup or switch, after a sufficiently long foreground absence, and on a 10-minute periodic freshness check. Each profile has at most one sync in flight. Transient failures use capped exponential delay with jitter; HTTP 429 waits at least 60 seconds. Authentication and other persistent failures stop automatic retries until an explicit refresh or credential change. Results carry profile identity so late events cannot replace another profile's view. A failed or partial Server traversal leaves the prior committed cache intact. This policy is a client freshness choice, not a Server snapshot guarantee or implicit payload download. The UI remains responsive because preflight, synchronization, artwork, SQLite and transfers run outside the egui thread.

## UI read model

Catalog grid and list share one filtered data set. Cards emphasize App name, optional artwork/description and local availability; missing optional metadata uses a neutral fallback. Details show all Releases and associations with individual download actions. Explicit platform/runtime/entry-point declarations are rendered as information, while raw exact JSON and IDs remain in Advanced sections. Library shows each verified local File and Tree reference with saved App and Release context without merging distinct profiles or associations. Downloads shows durable progress and per-member Tree details. UI status distinguishes cached metadata, current connection health, transfer state and verified local bytes. It never claims that a verified download is installed or launchable. No Release, channel, executable or primary Artifact is inferred.

## Trust boundaries

Limit response sizes, artwork dimensions/cache and concurrent transfers. Parse exact uint64 values. Validate inventories before materialization: no absolute, traversal, backslash, NUL, invalid UTF-8, duplicate, case-fold/normalization collision, special-file or destination-reserved names; use safe directory handles or equivalent anti-symlink checks. Reject an unrepresentable tree instead of renaming it. Only materialize verified bytes, and never execute them during validation. Render declaration text as inert text. Future argument arrays must be passed as separate argv elements, not interpolated into a shell; target and working directory must stay within the verified Artifact tree. See [LOCAL_LIBRARY.md](LOCAL_LIBRARY.md) for storage and recovery.
