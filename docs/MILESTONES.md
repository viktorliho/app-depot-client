# Milestones and acceptance

## M0 — foundation (this repository state)

Audit the Server contract/implementation and ArtCraft patterns; define boundaries, lifecycle gaps, local library, conformance plan and client governance; create a minimal buildable Rust workspace. No desktop app, installer or launcher is claimed. No Server change or remote publication.

## M1 — first functional client

M1.1 now provides the read-only HTTP transport and CLI: capability discovery, catalog traversal, Artifact metadata and declaration snapshots. It does not provide a desktop UI, transfer engine, local library or execution.

Implement a shared core transport, declaration reader, transfer engine and local library; expose a small diagnostic CLI and an egui desktop browser on Linux. Windows/macOS paths and adapters must compile or be clearly tracked as later platform validation. All M1 operations use existing Server read APIs. No install, runtime preparation or launch implementation.

Acceptance criteria:

1. User can add at least two independently keyed Server profiles, connect anonymously where allowed or with a Bearer token, read `/capabilities`, and see a specific incompatibility when required download features or identity formats differ. Token survives only in native secure storage; unavailable store offers session-only/anonymous mode.
2. Public and authenticated catalog lists Apps, Releases and every Artifact association with pagination. Private/inaccessible resources do not leak. Unknown platform/runtime and absent entry points remain visible; no Artifact is dropped because metadata is incomplete.
3. Details display App/Release/Association declaration data, all points and their assessment, and optional App artwork. Unknown JSON fields are preserved with semantically complete and exact values, including large integers, and are not rewritten or dropped by typed projection. Preserving original JSON whitespace or formatting is not required.
4. User can download any accessible single-file Artifact and a tree via complete inventory and per-file endpoints. Progress, queue, pause/resume, retry and actionable errors are visible; Range/If-Range and full-200 fallback work.
5. Every completed file passes SHA-256; every completed tree passes per-file hashes and Tree Identity v1. Unsafe or unrepresentable paths fail before materialization. Completion is atomic and survives process interruption/restart.
6. Completed downloads and cached metadata remain browsable offline, clearly marked with last sync. A lost Server authorization does not delete local bytes. Multiple profiles and associations with identical content IDs retain separate context.
7. UI distinguishes Downloadable, Downloading, Downloaded and verified, Installation required when explicitly established by a recognized future declaration, Installed/ready to launch only when actually established, unsupported platform/runtime/procedure and missing/invalid launch declaration. In M1, install/launch statuses are informational or unavailable; there is no functional Launch action.
8. CLI and GUI use the same core operations. Tests in [CONFORMANCE.md](CONFORMANCE.md) pass against isolated Dev/Test data; no Production data changes.

Recommended build order: (1) protocol models/capability negotiation and CLI read commands; (2) declaration and compatibility read model; (3) local profile/library schema and credential adapter; (4) file transfer/integrity/recovery; (5) tree inventory/path validation/identity; (6) egui browser/details/queue; (7) Linux packaging and platform validation. Keep each step testable and avoid adding UI-specific behavior to core.

## Later milestones

Only after a shared contract decision, implement one narrowly defined portable launch or installation method at a time with user consent, capability reporting, security validation and independent-client examples. No arbitrary script runner or automatic dependency installer is implied. Windows/macOS packaging and platform adapters follow verified core behavior.
