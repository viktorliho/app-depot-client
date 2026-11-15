# Client architecture (M0)

The Server stores authoritative declarations and immutable Artifact bytes. The client reads them, chooses only operations it understands, asks for appropriate user authority, and verifies every download. Local state records what this client has downloaded or performed; it never alters the Server's meaning. Independent clients can implement the same Server contract without this repository.

## Decision records

### ADR-001: Rust core, egui adapter

Use a native Rust core with an egui desktop UI on Linux first, then Windows/macOS. A CLI uses the same core for diagnostics and conformance. The current workspace contains only a minimal core identity foundation. Future modules/crates are introduced when concrete M1 responsibilities exist:

| Boundary | Responsibility |
| --- | --- |
| Protocol transport | HTTP, Bearer header, pagination, exact JSON/uint64 handling, RFC 9457 errors, capability negotiation; no UI or filesystem policy. |
| Domain/interpreter | App, Release, Association, Artifact and declaration scope; raw JSON retained beside typed recognized views; explicit support assessment. No filename-based inference. |
| Transfer engine | Durable jobs, bounded concurrent file transfers, Range/If-Range, SHA-256, Tree Identity v1, path safety, atomic completion. |
| Local library | Profiles, snapshots, jobs, verified content and offline views; storage transactions and recovery. |
| Install/runtime/launch ports | Future capability-aware implementations. Each exposes supported declaration versions/methods and a preparation/launch plan; UI cannot call arbitrary commands directly. No such implementation in M0/M1. |
| GUI/CLI | Commands and read models from core. GUI renders grid/table, details and queue; CLI exposes capability checks, catalog, validation and download diagnostics. |

Core exposes asynchronous commands and immutable state snapshots/events. UI thread never blocks on HTTP or hashing. Background operations publish progress and durable transitions. The CLI is a first-class consumer, so a future TUI, mobile frontend or Decky client would not require splitting logic out of egui. The protocol itself remains language neutral.

### ADR-002: Explicit identity and selection

Generate a stable local `ServerProfileId` on profile creation; key all Server-local records by it and the Server ID. URL is editable and never used as identity. Keep separate App, Release, Association, Artifact and declaration revision IDs. A shared Artifact digest may reuse verified bytes internally, but access and display context remain profile/association scoped. Do not merge two Server profiles because their digests match.

Show every eligible Release and every association. Optional channels are labels/pointers; no implicit `stable` or `latest`. A Release version is display text, not a total ordering key. Platform-free Artifacts remain visible and downloadable. Unknown platform tokens are visible as unassessed; only recognized, explicitly mapped tokens can produce a compatibility verdict. No association is “primary” unless a future shared declaration explicitly establishes that concept.

### ADR-003: Download is independent of installation and launch

Model `Download`, `Install`, `RuntimePreparation`, `Launch` as distinct requests and records. `DownloadedVerified` means content integrity only. A declared entry point has a target, but current Server conventions do not specify execution mechanism, arguments or runtime preparation. It does not make a Launch action functional. Keep all unsupported declarations visible with an explanation. Treat declarations as metadata, not security authorization: even future supported operations require user consent and local security checks.

### ADR-004: Capability-aware compatibility

At connection, read `/capabilities`; retain unknown fields, check the API major/version policy and identity formats actually required for requested operations, and check `downloadRanges`. Never infer support from the Server product version alone. If a capability is absent or incompatible, show catalog where safely possible and disable only affected operations. Do not treat current `apiVersion: 1.0.0` as a stable product 1.0 guarantee. Future declaration operations need their own versioned, shared vocabulary; unknown versions remain opaque.

### ADR-005: Credentials

Use the OS secure store (Secret Service on Linux, Credential Manager on Windows, Keychain on macOS) for Bearer tokens, indexed by local profile ID. Keep profile metadata and token references in the library database; never serialize tokens into jobs, logs, crash reports or exports. If secure storage is unavailable, offer only session-memory tokens or anonymous access. An explicit user opt-in may use a passphrase-encrypted local vault in a later milestone after its format and recovery policy are specified; plaintext fallback is prohibited. Use HTTPS for remote servers by default; allow loopback HTTP for local development and explicit user override for self-hosted LAN endpoints with a clear transport warning. Do not send credentials across a redirect to another origin.

## UI read model

Library grid and sortable table share one filtered data set. Cards/rows show App artwork if available, Release/Artifact choices and status. Details show all associations, exact platform/runtime declarations, entry points, and unsupported/unknown fields in readable raw JSON. Queue shows bytes, per-file progress, validation, pause/resume/retry and problems. Updates compare a locally pinned Release/channel selection to current Server data only when online; no guessed semantic-version ordering. Status is a set of facts, not a single lossy enum: transfer state, integrity state, install state, compatibility assessment and launch support can coexist. The UI labels include Downloadable, Downloading, Downloaded and verified, Installation required, Installed/ready to launch, Unsupported platform/runtime, Unsupported installation procedure, and Missing/invalid launch declaration. Only display Installed/ready to launch after a future supported installer or portable-launch assessment has actually established it; M1 has no Launch button.

## Trust boundaries

Limit response sizes, artwork dimensions/cache and concurrent transfers. Parse exact uint64 values. Validate inventories before materialization: no absolute, traversal, backslash, NUL, invalid UTF-8, duplicate, case-fold/normalization collision, special-file or destination-reserved names; use safe directory handles or equivalent anti-symlink checks. Reject an unrepresentable tree instead of renaming it. Only materialize verified bytes, and never execute them during validation. Render declaration text as inert text. Future argument arrays must be passed as separate argv elements, not interpolated into a shell; target and working directory must stay within the verified Artifact tree. See [LOCAL_LIBRARY.md](LOCAL_LIBRARY.md) for storage and recovery.
