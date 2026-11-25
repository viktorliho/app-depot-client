# Declarative software lifecycle

The Server's [Native portable launch v1](https://github.com/viktorliho/app-depot-server/blob/main/docs/architecture.md#native-portable-launch-v1), API and OpenAPI define the shared protocol. App, Release and Association declarations remain independently versioned JSON documents. The client preserves their full values, including unknown fields and exact JSON integers. An Artifact's bytes, name and executable flag never create software semantics.

## Current supported operation

An Association `data.appDepot.entryPoints[]` item may carry `launch: {"method":"NativePortableV1","workingDirectory":"System64","arguments":[]}`. Its existing `target` is the only executable target: `ArtifactFile` for a File Artifact or an exact `TreeFile.path` in verified inventory. A point without `launch` is a non-executable file reference. The method is a direct native process operation; it is not an installer, shell script, runtime preparation instruction or permission grant.

The point must have nonempty `platforms`. Known v1 tokens are `linux/x86_64`, `linux/aarch64`, `windows/x86_64`, `windows/aarch64`, `macos/x86_64` and `macos/aarch64`. Matching is exact and case-sensitive. A nonempty Release `data.appDepot.platforms` further intersects the point's known set. Unknown tokens remain stored but have no v1 match. Only Linux x86_64 and aarch64 have an execution adapter in the reference client; Windows/macOS declarations remain readable and inert here.

`workingDirectory` is relative to the Artifact root. Omission uses the target file's parent; the empty string selects the root. For a File Artifact, only root is valid. A nonempty Tree directory must be present as a Directory entry in verified inventory. `arguments` defaults to `[]`; each item is a literal UTF-8 argv string, including possible empty strings. There are at most 64 arguments, at most 4096 UTF-8 bytes per item and 16384 bytes total. Controls, shell parsing, substitutions and environment expansion are unsupported.

The client reads a consistent cached Release declaration snapshot and keeps each profile, App, Release, Association, Artifact and entry-point ID separate. It assesses all points; no filename, array order, channel or version picks a default. If several compatible points exist, the user selects an Association and entry-point ID explicitly. A newer synchronized declaration replaces the cache for future assessment; a desktop Play command is bound to the displayed Association declaration revision and rejects a changed revision instead of silently changing targets. Offline launch can use a cached snapshot, a completed verified download reference and a prepared local installation without contacting the Server. Cache age and local integrity remain distinct.

Before Linux execution, the client requires a completed verified local Artifact reference for the selected Association and a separately prepared writable installation. Preparation checks immutable file objects against SHA-256, attempts independent reflink copies and falls back to ordinary copies, then records a local installation identity. This happens automatically after a compatible download or transparently on first Play for an older download. A declared ArtifactFile is made executable in that local installation; this does not change the immutable source or infer execution from its bytes. Tree members retain their declared portable executable flags. The client checks the declared target and working directory against verified inventory, opens runtime path components without following symlinks, and checks regular-file type and executable mode. It does not compute the installed executable's SHA-256 or compare the installation with the original Artifact on every Play. User modifications, patches, configuration and mods are permitted. A direct native process is started from the opened inode with the declared working directory and literal argv; App Depot token, secret and SSH-agent environment variables are removed. The UI does not block while it starts or waits. Closing the launcher does not intentionally terminate a running child. Every Play is explicit user intent.

**Verify integrity** is a separate read-only local operation. It compares expected files with the published Artifact inventory and reports missing, modified and unexpected paths. **Repair** is a separate confirmed operation: it preserves the former installation, including user modifications and additions, in managed staging, then restores original files from verified immutable objects without changing managed user data. Verification does not repair anything, and Play does not silently verify or repair. Download verification proves delivered bytes match the Server's content identity; it does not certify software safety or later user changes. The mutable installation is Play-ready when its declared target is safe and executable, even if it differs from the original Artifact. Retaining the immutable reference allows offline repair, but consumes storage; discarding that cache reference independently of an installation needs additional reference and recovery work.

Unknown methods, malformed declarations, platform mismatches, missing installed targets, unsafe paths and non-executable targets remain visible as unavailable. Damaged immutable bytes prevent preparation or repair until restored; modifications within an existing safe writable installation do not by themselves block Play. These limitations do not prevent browsing or opening a local folder. The Server's read-only target/launch assessment informs publisher editing but does not establish client execution readiness or authorization.

## Remaining contract gaps

Release `runtime.name/version` is descriptive only. InstallationManifestV1 supports Standard payload composition; the shared Core journals prerequisite authorization and outcomes; native Linux prerequisite execution, Derived installation, Wine/Proton, environment override and system package operations remain unsupported. Data-only Artifacts need no launch metadata. Any future executable method requires a separate optional shared Server contract and explicit client support; arbitrary shell commands are not part of this method.

The CLI installation plan reader uses `--profile ID plan manifest RELEASE`,
`plan resolve RELEASE PLATFORM REVISION|current SELECTIONS_JSON`, `plan list`
and `plan inspect ID`. Inspection and listing read the local library without a
Server connection. Resolution saves metadata only; prerequisites, launch and
Derived recipe locks do not authorize any action. A Release explicitly declaring
an installation manifest cannot use legacy NativePortableV1 preparation or Play.

On Linux, `install PLAN_ID` explicitly acquires and installs the validated
Standard payload (see [Local library](LOCAL_LIBRARY.md#standard-installation-materialization-boundary)).
It never launches automatically. `launch list RELEASE_ID` projects the active
generation's saved launch selection, and `launch play RELEASE_ID ASSOCIATION_ID
ENTRY_POINT_ID` executes only that declared installed target using the existing
no-follow NativePortableV1 process adapter. The choice preserves the Association
revision and separately carries the generation UUID, which guards against a
changed active generation before Play. Modified installed bytes are allowed;
Verify is a separate operation. A new generation with prerequisites is not
active until the lifecycle completes; a previous ready generation remains
launchable. Inspect the exact plan with `install requirements PLAN_ID`, then
use `install confirm GENERATION_ID FINGERPRINT SELECTED_IDS_JSON --confirm`
after reviewing all required and optional installers. The JSON array explicitly
selects IDs; omitted optional IDs are declined, and omitted required IDs are
rejected. `install progress GENERATION_ID` displays the saved plan and durable
rounds. `install retry GENERATION_ID FINGERPRINT SELECTED_IDS_JSON --confirm`
authorizes a new processing round after review; normal confirmation cannot
silently retry an attempted round. `install process GENERATION_ID CONSENT_ID
--confirm` consumes the round. Linux has no native installer adapter: selected
execution is unavailable and cannot produce success, while an optional-only
plan can be explicitly declined and activated without launching anything.
The shared executor interface is available for the native Windows integration.
No launch selection means no Standard launch candidate. Windows projects the
active generation and binds its declared installed target to the existing NTFS
launch checks. Play records generation-specific supervision before resuming the
process. Running or uncertain occupancy blocks another Play of that generation.
Windows Standard removal uses the same ownership inspection and no-replace
quarantine as legacy Windows runtime removal, with durable generation-specific
intent, quarantine and cleanup records. It refuses modified or unexpected
content and uncertain process or prerequisite occupancy.
