# Milestones and acceptance

## M0 — foundation

Audit the Server contract/implementation and ArtCraft patterns; define boundaries, lifecycle gaps, local library, conformance plan and client governance; create a minimal buildable Rust workspace. No desktop app, installer or launcher is claimed. No Server change or remote publication.

## M1 — first functional client

M1.1 introduced the read-only HTTP transport and CLI: capability discovery, catalog traversal, Artifact metadata and declaration snapshots.

M1.2 introduced persistent local Server profiles, native secure-store token references, transactional catalog synchronization, and offline cached Apps/Releases/declaration snapshots through the CLI. URL changes create a new profile identity; archived profiles retain their cached catalog. Linux secure storage and filesystem behavior have been exercised; Windows/macOS adapters still require platform validation.

M1.3 implemented single-file Artifact transfer through the existing content endpoint. Jobs retain profile and association context, validated checkpoints, strong ETags, and durable states. The CLI starts, pauses, resumes, retries and cancels transfers, then lists SHA-256-verified files offline. Range resume, full-200 restart, 416 recovery, staged integrity checks, object reuse and crash reconciliation are implemented.

M1.4 implements Tree Artifact inventory traversal, canonical Tree Identity v1, per-file verified/resumable transfers, explicit empty directories, portable executable flags and atomic Linux materialization. The same download commands manage both Artifact kinds; `download trees` and `download tree-files JOB_ID` expose offline results and per-file progress. Interrupted inventory fetches restart, completed member objects are reused after SHA-256 validation, and interrupted Committing jobs are reconciled. Linux process interruption and real Server compatibility have been validated. Windows/macOS atomic directory promotion and filesystem guarantees remain unimplemented or unvalidated, so Tree completion is disabled there. Installation and execution remain future work.

M1.5 implements a native egui desktop app with Catalog grid/table and details, offline Library, durable Downloads and profile/credential Settings. It shares the existing core with the CLI; HTTP, SQLite, image decoding and hashing run outside the UI thread. Artwork comes only from bounded Server artwork endpoints. File and Tree associations remain separate download choices. Linux Wayland/X11 desktop smoke checks use an isolated Dev/Test catalog and a disposable library. There is no Install or Launch action, background daemon or release packaging. Windows/macOS desktop behavior remains to be validated on those hosts.

M1.5.1 redesigns the desktop around first-run Server connection, guided personal-token setup, automatic offline-first catalog refresh, responsive App cards/details, an App-centric verified Library, readable durable Downloads and grouped Settings. System theme and compact presentation preferences persist locally without storing credentials. No automatic Artifact download, installation or launch is introduced.

M1.5.2 adds bounded live transfer observations, measured speed and conditional ETA, Tree member counts/current path, clearer processing states and actionable errors in Downloads. It reduces small-file latency on the measured HTTP Dev/Test Server and avoids repeated full Tree progress scans and unnecessary small-member staging synchronizations. Recovery remains based on durable SQLite checkpoints and verified objects. Tree transfer remains sequential; Windows/macOS performance and desktop behavior remain unvalidated.

M2.1 introduced the optional Server NativePortableV1 contract in the shared core, CLI and Linux desktop. Current launch assessment uses cached Release snapshots and completed verified download references without inventing entry points, resolves exact File/Tree targets and working directories without following symlinks, and reports process exit. M2.2 added a separate writable installation, and M2.2.1 removed automatic hashing of installed executables on Play. Windows/macOS launch adapters and actual game-specific compatibility remain separate work.

M2.2 makes a separate writable Linux runtime copy for a verified NativePortableV1 Association, keeping immutable source objects out of application writes. New Linux libraries default to `~/.appdepot`; marked older XDG libraries remain selected until an explicit source-preserving migration. Local runtime, Association download and App-scoped removal are available in CLI and desktop with confirmation. Per-item source inspection prevents one damaged Tree from hiding other items, and explicit Tree repair uses verified local objects while quarantining the old directory. SQLite schema version 4 records runtime and removal state. Shared file objects are reclaimed only when no durable local owner remains. There is no OS-level sandbox, automatic cleanup of external application data, or Windows/macOS runtime preparation/removal adapter.

M2.2.1 adds a durable, prioritized library-wide download queue with checkpoint-bound preemption and automatic resumption of scheduler-paused jobs. New NativePortableV1 downloads prepare writable installations automatically; older verified downloads prepare transparently on first Play. Version 5 records queue rank and automatic-resume intent. Connected Browse hides Apps absent from a completed synchronization while cached metadata and local content remain available offline. Installed programs are user-modifiable: Play checks the declared target and path safety but does not hash installed bytes. Explicit Verify integrity reports changes, and confirmed Repair restores original files from verified local objects while preserving the previous installation in managed staging. Download-time Artifact integrity and immutable source verification remain mandatory. The desktop shows operation state during preparation and removal; no automatic execution or Server mutation is introduced.

M2.3 reorganizes the native desktop presentation around a Linux custom titlebar, compact navigation, shared light/dark styling, larger readable text and a persisted bounded interface scale. Browse, details, My Library, Downloads and Settings use the same visual components. Multiple launch choices and all Release associations remain explicit; completed transfer history is collapsed by default, with durable records unchanged. This is a presentation milestone: no Server contract, transfer integrity, writable-runtime policy or CLI command semantics change. Windows/macOS keep native window decorations until tested.

M2.3.2 refines the existing desktop views: My Library uses stable compact cards and list rows with Play or Open Folder as the visible action, while right-click and More open the same per-Association management menu. Browse list reserves space for local status and adapts at narrow widths. App details can lead with Play for one uniquely applicable local candidate across several Release choices, with the exact choice shown beside it; several candidates still require selection. Downloads separates queue title, saved progress and trailing actions, and retains compact history. This changes presentation and command access only; the core lifecycle, Server declarations and CLI remain unchanged.

M2.4 introduces the official Linux x86_64 AppImage distribution, pinned packaging inputs, CI and intentional tag-driven GitHub release workflow. The desktop embeds version, source revision and build kind, shows the selected App Depot icon in its titlebar and native window, and checks newer stable official releases independently of Server profiles. A supported user-owned AppImage can download a verified update and use a separate helper for atomic activation and rollback after a failed start. Development and unsupported installations cannot self-replace. See [distribution and update details](DISTRIBUTION.md).

M2.4.1 fixes interactive resizing of the undecorated Linux window. Edge and corner detection uses the pointer's current viewport position, supplies a resize cursor and requests compositor-managed resizing through eframe/winit. The custom titlebar and minimum size remain. The v0.1.1 AppImage uses the same desktop identity and stable update channel as v0.1.0; no library or Server contract changes are involved.

The original M1 objective was a shared core transport, declaration reader, transfer engine, local library, diagnostic CLI and egui desktop browser on Linux. All M1 network operations use existing Server read APIs. M1 did not include installation, runtime preparation or launch; M2.1 adds only the separately declared native portable launch method.

Acceptance criteria:

1. User can add at least two independently keyed Server profiles, connect anonymously where allowed or with a Bearer token, read `/capabilities`, and see a specific incompatibility when required download features or identity formats differ. Token survives only in native secure storage; unavailable store offers session-only/anonymous mode.
2. Public and authenticated catalog lists Apps, Releases and every Artifact association with pagination. Private/inaccessible resources do not leak. Unknown platform/runtime and absent entry points remain visible; no Artifact is dropped because metadata is incomplete.
3. Details display App/Release/Association declaration data, all points and their assessment, and optional App artwork. Unknown JSON fields are preserved with semantically complete and exact values, including large integers, and are not rewritten or dropped by typed projection. Preserving original JSON whitespace or formatting is not required.
4. User can download any accessible single-file Artifact and a tree via complete inventory and per-file endpoints. Progress, queue, pause/resume, retry and actionable errors are visible; Range/If-Range and full-200 fallback work.
5. Every completed file passes SHA-256; every completed tree passes per-file hashes and Tree Identity v1. Unsafe or unrepresentable paths fail before materialization. Completion is atomic and survives process interruption/restart.
6. Completed downloads and cached metadata remain browsable offline, clearly marked with last sync. A lost Server authorization does not delete local bytes. Multiple profiles and associations with identical content IDs retain separate context.
7. UI distinguishes Downloadable, Downloading, Downloaded and verified, Installation required when explicitly established by a recognized future declaration, Installed/ready to launch only when actually established, unsupported platform/runtime/procedure and missing/invalid launch declaration. In M1, install/launch statuses are informational or unavailable; there is no functional Launch action.
8. CLI and GUI use the same core operations. Tests in [CONFORMANCE.md](CONFORMANCE.md) pass against isolated Dev/Test data; no Production data changes.

## M3.0 — SteamOS architecture and feasibility

[ADR-016](ARCHITECTURE.md) retains Linux Desktop and prioritizes SteamOS Decky, then Windows Desktop and macOS Desktop. Steam is the SteamOS library and exposes every App accessible to the configured Server profile. All clients use the existing App Depot protocol.

M3.0 includes a headless First Play CLI with explicit cached selection, shared transfers, verified runtime preparation and supervised native launch. A disposable loopback Dev Server harness covers download and offline execution after installed-file modification. Binary VDF reconciliation and artwork derivatives are exercised with disposable fixtures. Decky packaging and Gaming Mode validation were deferred to M3.1.

## M3.1a — SteamOS Decky release candidate

The installable local candidate adds Decky onboarding, catalog reconciliation, artwork, a shared-core native launcher and a pinned single-file installer. Build, verification and deployment boundaries are in [SteamOS Decky](STEAMOS_DECKY.md). The owner later validated the MVP flows listed in [SteamOS MVP status](STEAMOS_DECKY.md#steamos-mvp-status).

## M3.1b — SteamOS integration

1. Validate the live Steam adapter and per-account binding ledger. Verify immediate appearance, repeated sync, equal names, rename, metadata changes and restart with an authorized Deck and disposable Steam account. Include uninstalled and unsupported Apps. Require stable AppIDs, readback, no duplicates and preservation of foreign shortcuts and user settings. Verify interrupted creation and Decky reload. Consider an offline VDF fallback only after safe shutdown/write behavior is established.
2. Install and upgrade the packaged wrapper and Decky panels on hardware. Test user-level credentials in Gaming Mode, plugin upgrade and reload, concurrent Play/Sync/Desktop queue behavior and internal and microSD library roots.
3. Verify native First Play and offline launch in Gaming Mode. Cover optional choice, controller navigation, readable progress, checkpoint retry, cancellation and offline Retry/Exit. Test focus, overlay, Steam Input layouts, built-in controls and Steam Controller, playtime, Stop Game, child and descendant lifetime, and return to Steam.
4. Verify live artwork independently. Cover source revision changes, original colors after install, user overrides, per-role scaling and formats, refresh after restart and operation without artwork.
5. Spike Steam-managed Proton for a benign Windows fixture using stable and mutable targets. Record actual AppID, compatibility tool, prefix, controller settings and first-session behavior. Keep Windows execution unavailable if neither path preserves the required experience. Keep Windows Apps visible either way.

## Later milestones

Installation, runtime preparation and any additional launch methods require separately authorized, optional shared contracts and client implementations. No arbitrary script runner or automatic dependency installer is implied. Windows/macOS packaging and native launch adapters follow verified platform-specific behavior.

## M3.2 — Decky release verification and native installation-state feasibility

M3.2 evaluated Gaming Mode release discovery and on-demand verification. M3.5 retires those controls and adopts the official manual Desktop installer. All SteamOS installation-tab work remains deferred, including native state changes, default tab replacement, TabMaster integration and App Depot-specific tabs. The existing non-Steam shortcuts and grayscale-to-color artwork behavior remain in use. [Deferred tab alternatives](STEAMOS_DECKY.md#deferred-alternatives-from-the-tabmaster-investigation)

## M3.3 — Decky Local Application Uninstall

Decky Installed provides a confirmed, fully local complete uninstall by App and Server profile. The transaction removes runnable copies, managed data and download references. Shared objects survive while another local owner remains. The Steam shortcut and its settings stay intact, and the owned grid artwork is refreshed separately. The next Play follows normal First Play. Secure Gaming Mode update activation remains deferred. [SteamOS scope and acceptance](STEAMOS_DECKY.md#m33-local-application-uninstall)


## M3.4 — Deferred Gaming Mode self-update

The project investigated NonSteamLaunchers-style script updates, Loader-managed installation, a restricted privileged helper and versioned user-space runtimes. Debian fixtures verified signed manifests, protected staging, exchange, failure recovery and rollback. The installed Loader and its tested upstream revisions exposed unprotected paths and lacked pre-replacement validation and reliable reload rollback. These findings and fixtures remain as research. [M3.4](https://github.com/viktorliho/app-depot-client/commit/385065c2e8f9353600d778e4dba27b8c27dd171d) · [M3.4.1](https://github.com/viktorliho/app-depot-client/commit/6987094eeecdbe84db4192fa9a3545a345ecfd1b) · [SteamOS research](STEAMOS_DECKY.md#deferred-gaming-mode-update-research)

## M3.5 — Decky maintenance and Windows handoff

The SteamOS Decky MVP enters maintenance. Settings & About shows the Decky version, official Releases link and diagnostics. The supported lifecycle uses the official version-pinned Desktop Mode installer. Steam remains the library and Play surface. Decky Installed handles Uninstall. The owner-tested native lifecycle and remaining hardware limits are recorded in the SteamOS guide. Linux Desktop remains the existing AppImage implementation, Windows Desktop is the next major platform, and macOS follows. [Manual updates](STEAMOS_DECKY.md#manual-installation-and-updates) · [SteamOS MVP status](STEAMOS_DECKY.md#steamos-mvp-status)

## M4.0 — Windows Desktop Architecture & Cross-Platform Core Audit

Scope is an audit and architecture plan. Preserve the existing Server protocol, Rust core, egui frontend and visual design. Target feature consistency with Linux Desktop through platform adapters, with unsupported operations gated until their safety properties are demonstrated.

| Surface | Starting point and Windows audit question |
| --- | --- |
| Filesystem and library | `library.rs`, `transfer.rs`, `tree_download.rs`, `migration.rs` and `managed.rs` use Unix permissions or Linux-specific no-follow handles, mount checks, atomic Tree promotion, reflinks and removal. Determine Windows handle-relative access, reparse-point rejection, directory promotion and durability on NTFS. Define behavior for case folding, reserved names, ADS, long paths, ACLs, concurrent access and sharing violations. Review `fs2`, `libc`, bundled SQLite and `directories` per target. |
| Process and runtime | `launch.rs` and `managed.rs` use Linux execution handles, `/proc` and Linux process tracking. Specify constrained executable and working-directory resolution, literal argument and environment handling, active process tracking and safe runtime cleanup. Scope audit to supported native executable declarations. Unsupported launch methods remain inert. |
| Credentials and data | `keyring` selects Secret Service, Windows native and Apple native features. Validate Windows Credential Manager lifecycle and account isolation. Decide per-user library, cache and preference locations, ACL creation, offline operation and recovery. Preserve opaque references and avoid plaintext fallback. |
| Shared desktop and distribution | `crates/app-depot-desktop` currently enables eframe/egui with X11 and Wayland features and hosts the AppImage updater. Audit Windows renderer/window behavior, platform preference persistence and isolation of AppImage scripts, helper and stable release channel. Evaluate signed Windows x64 packaging and upgrade/rollback requirements before choosing a format. |

Use a disposable Windows x64 VM or runner with a pinned Rust MSVC toolchain and Windows SDK, NTFS fixture volumes, separate non-admin test identities and an isolated Dev/Test Server. Keep Linux CI as the reference. Use native Windows tests for paths, credential isolation, process lifetime and interruption where cross-compilation cannot supply evidence.

M4.0 acceptance is a source-referenced dependency and capability matrix, a prioritized adapter plan with tested or explicitly unverified assumptions, a reproducible Windows build/test environment proposal, and acceptance fixtures for portability, secure credentials, filesystem safety and packaging. Windows client implementation and release are separate milestones.

## M4.0b — Native Windows catalog preview

The Windows x64 Desktop uses the existing egui views and Rust core. The Windows eframe renderer uses Direct3D 12 through wgpu. The Linux Desktop continues to use Glow, and the AppImage updater is Linux-only.

Build from a Visual Studio 2022 x64 developer prompt with the C++ Build Tools and Windows SDK installed:

~~~bat
rustup toolchain install 1.100.0 --profile minimal --component rustfmt --component clippy
rustup target add --toolchain 1.100.0 x86_64-pc-windows-msvc
cargo +1.100.0 build -p app-depot-desktop --bin app-depot-desktop --target x86_64-pc-windows-msvc --locked
~~~

The executable is at x86_64-pc-windows-msvc/debug/app-depot-desktop.exe under Cargo's target directory. Windows stores the profile database and desktop preferences under %LOCALAPPDATA%\app-depot-client by default. Set APP_DEPOT_LIBRARY_DATA_DIR to an absolute path in a private disposable directory before launching a test instance. Artwork is cached in memory. The profile stores only an opaque reference to any token saved in Windows Credential Manager.

Windows supports profiles, catalog sync, artwork, release details, preferences and offline catalog browsing. Transfer scheduling and recovery, artifact download, installation, Play, repair, removal, storage management and AppImage updates are unavailable on Windows. Enabling these operations requires verified handle-relative traversal and reparse-point rejection, atomic publication and recovery on NTFS, and native process identity and cleanup rules. macOS requires its own verification of those guarantees and its existing Keychain backend.

## M4.1 — Windows filesystem foundation

The Windows core opens fixed drive roots and walks one validated component at a time relative to directory handles. It refuses reparse points and ambiguous components, including drive-relative input, DOS device aliases, alternate data streams and trailing dots or spaces. The Windows catalog library uses these operations to create its directories and read or exclusively create its identity markers. These operations inherit the parent ACL. Use an owner-controlled library root. SQLite and other catalog metadata still use their existing persistence mechanisms.

The directory promotion primitive submits a single handle-based no-replace rename to NTFS. It requires a known case-insensitive destination directory, preserves the staged tree on collision or sharing failure and does not fall back to copying across volumes. Visible atomic promotion and component safety are distinct from power-loss durability. A successful directory flush on this VM alone does not establish the ordering of directory entries, CAS objects and SQLite journal commits across failure.

Windows File and Tree transfer execution and startup reconciliation remain gated in Core and Desktop. Safe relative file creation and directory promotion do not yet provide the full CAS workflow. Required work includes handle-relative File and Tree materialization and cleanup, durable visibility and recoverable commit ordering, and compensation after a partially published operation. Native Windows launch additionally needs executable and working-directory binding, literal arguments, sanitized environment, process identity after restart, and removal exclusion while running. Linux download, install, recovery and execution remain in their existing paths. Tree Identity v1 is unchanged.

The Windows workspace skips File fixtures covering HTTP resume, invalid range/validator and corrupted staging, checkpoint and priority behavior during transfer, observation accounting, zero-size and ordinary completion, CAS reuse, retry/cancellation, commit failure, and interruption. Those fixtures require filesystem staging and publication guarantees. The Tree disk fixtures cover staging, checkpointing, completed materialization, commit recovery, collision and locking. They remain enabled outside Windows. Canonical identity vectors, catalog persistence, credential storage, NTFS name validation, junction refusal, exclusive file creation and no-replace directory promotion are tested natively.

### M4.1a — CAS publication groundwork

Protocol invariants are exact Artifact SHA-256, canonical Tree Identity v1 and verified inventory. The existing persisted contract records synchronized prefix checkpoints, enters Committing before publication and marks Completed only after local content is visible. Linux realizes these requirements using filesystem-specific locks, no-replace links or renames, and file and directory synchronization. Windows retains the same protocol and schema.

The shared File and Tree paths now call the Windows adapter for staging opens and CAS links. The adapter verifies content through the same opened handle used for the NTFS no-replace hard link. It denies concurrent writers while hashing and linking, rejects reparse traversal and queries case handling on the destination directory. It performs no cross-volume copy fallback. A writable directory handle supports explicit flushing. Process termination fixtures cover staged bytes before linking and reopening complete bytes after linking and flushing. They do not cover the SQLite commit or an unexpected power loss.

The Windows execution and UI gates remain active. The lifecycle still needs ownership-safe staging cleanup, exclusion of mutable aliases after linking, recovery across CAS and SQLite commit boundaries, and handle-relative Tree verification and installation. Test physical power loss separately on an isolated VM snapshot or disposable dedicated NTFS volume. Enable Download and Install only after validating transaction invariants, verification, rollback and recovery with disposable File and Tree artifacts. Play and Uninstall remain unavailable.

#### M4.1a — Independent publication and owned temporary cleanup

Windows publication now makes an independent copy in the destination directory on the same volume. It rejects an open source writer, checks the copy through its exclusive handle, flushes it and atomically renames it without replacement. Writable content opens reject files with multiple NTFS links, including legacy staging aliases. App Depot retries can change their own staging bytes without changing the published copy. The library still assumes private storage and does not protect against external same-user processes with direct write access to the library.

On publication failure App Depot disposes only the exclusive temporary creation handle, including on hash rejection and destination collision. It never reopens a temporary filename to delete it. If only an old name survives, the Windows File path retains it and the Tree path refuses to recursively remove the old work directory. Process tests distinguish a flushed source before publication, an unpublished temporary copy after interruption, and a published object after rename and directory flush. Retry leaves unidentified temporary objects untouched. There is no ownership record for reclaiming those orphans across a crash.

The SQLite schema and protocol remain unchanged. Tests stop short of the production Windows journal: they do not execute File or Tree job reconciliation, register verified content across commit boundaries, activate an installation or prove rollback. Download, Install and Verify remain gated pending those guarantees. A process restart test does not establish physical power-loss ordering.

#### M4.1a.1 — File download, installation and recovery

File requests stream into the existing staging slot. Persisted checkpoints bind the synchronized prefix to its validator and SHA-256. A resume validates the prefix and uses Range/If-Range. The job enters `Committing` only after validating the complete size and digest. Windows closes the staging writer before publishing independent, flushed copies in CAS and at the verified-download location using no-replace renames. The verified-content reference and `Completed` state are written in one SQLite transaction after publication. Recovery revalidates the CAS bytes and can complete the transaction after interruption. An uncommitted verified-content insert rolls back with the job update.

An explicit File install revalidates the completed download and records `Preparing` in the existing installation journal. Windows builds an independent payload and installation ID marker in an exclusive directory, flushes the files and staging directory, and publishes the directory using an atomic no-replace rename. Activation verifies the marker, size and digest, flushes the destination parent and commits `Ready`. A restart can activate a valid published `Preparing` installation. An absent destination waits for explicit retry. An unexpected destination is preserved. Verification covers the installed payload and marker, and revalidates the verified download. A failed activation leaves `Preparing` and can be retried. No part of the File path removes unidentified directories or staging artifacts left by a prior process.

Process-termination tests exercise both sides of CAS publication, an uncommitted verified-content insert, committed download completion, committed installation intent, final placement and committed activation. These tests exercise SQLite and NTFS recovery after process exit. Physical power-loss durability requires independent validation on disposable storage. The managed root must remain private to the current user. Independent publication excludes aliases owned by App Depot. External processes with access to the library can still change files. Verification rejects changes to the payload. Tree download and installation, Repair, Play and Uninstall remain unavailable on Windows.
