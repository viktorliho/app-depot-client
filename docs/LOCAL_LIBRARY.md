# Local library and recovery contract

## Layout and identity

New Linux libraries use `~/.appdepot`. A marked older XDG library remains selected until an explicit migration; distinct marked libraries at both default locations require explicit root selection. Windows uses AppData and macOS uses Application Support. Metadata and content roots may still be configured separately. The combined Linux layout is:

```text
library.sqlite       local profiles, snapshots, jobs and verified locations
desktop-preferences.json  non-secret desktop preferences
apps/<local-id>/    independent writable portable runtime copies
objects/             immutable verified file bytes, keyed by digest
artifacts/           completed user-visible file/tree materializations
staging/<job-id>/    partial bytes, inventory snapshot and resume journal
userdata/<local-id>/ App Depot-managed application data
artwork/             bounded, validated display cache
```

Paths are illustrative local implementation details, not Server protocol fields. Persist `ServerProfileId`, normalized connection URL, display name, capability snapshot, token reference, App/Release/Association IDs, Artifact identity, declaration raw JSON and revision/ETag, optional channel snapshot, artwork metadata, download jobs, completion manifests and timestamps. Store content length and digests as exact 64-bit integers/hex; no JSON floating-point conversion. One profile may be offline, deleted remotely or inaccessible while its completed local copies remain available. Show cached metadata with last-sync time and stale status. Do not claim a local copy is still authorized by the Server.

The desktop app reuses this database and the same validated metadata/content roots as the CLI. Its separate `desktop-preferences.json` beside the database stores only theme, grid/list preference and last selected profile ID; it contains no credentials. The file is bounded, owner-only on Unix and replaced atomically; unsafe symlinks are rejected. The app keeps decoded artwork textures in memory only for the current session, keyed by profile, App, role and Server binding; a completed catalog sync invalidates them. Restarted offline browsing retains cached catalog metadata and verified Artifact paths but uses artwork placeholders until the Server is reachable again. Starting a download may refresh individual metadata records; this does not advance the last complete catalog synchronization marker.

### Implemented M1.2 foundation

`LibraryPaths` uses `~/.appdepot` for a new Linux library and keeps a previously marked XDG default active until explicit migration; AppData and Application Support remain the Windows/macOS defaults. CLI flags/environment can select the metadata root and content root independently. The metadata root contains `library.sqlite`; the content root contains prepared `objects/`, `artifacts/`, `staging/`, `artwork/`, `apps/`, and `userdata/` directories. Both roots carry the same `.app-depot-library-id` UUID marker so a data root cannot silently be paired with another content root. On Unix, newly created directories are private and the database is mode `0600`; existing roots/databases with group or other access are refused rather than silently changed. Root paths must be absolute and contain no symlink components or `.`/`..`. Initialization creates only its named entries and refuses unknown existing SQLite schemas. Windows/macOS filesystem validation still needs platform testing.

Schema version 1 stores profiles, App/Release/association/Artifact metadata JSON, and Release declaration snapshots with scope revision IDs and snapshot ETags. JSON uses exact integer parsing and preserves unknown properties. The schema reserves minimal `verified_content` and `download_jobs` identity/state tables; M1.2 does not write them or mark any content verified. Timestamps are Unix milliseconds. Every successful sync writes fetched metadata and the profile's capabilities/status/timestamp in one SQLite transaction. A failed fetch leaves the prior catalog and last successful sync intact. Cached rows retain their last-seen time; `stale` means absent from the latest completed sync, while `source: cache` always distinguishes them from a live API response. Successful synchronization never deletes unseen rows, since remote publication and access can change. A Release snapshot is internally consistent, but the Server does not expose a globally atomic catalog snapshot.

Changing an origin creates a new profile ID and archives the old profile/cache. Explicit profile removal also archives and retains its metadata for offline browsing, while removing its secure-store token. Archived profiles cannot synchronize. The CLI offers no purge in M1.2. Tokens are stored only in native secure storage or process memory. If Secret Service, Credential Manager or Keychain is unavailable, the client uses anonymous access or a temporary `APP_DEPOT_TOKEN`; it never falls back to plaintext storage. Native storage was validated on the Linux development host; Windows/macOS backends remain untested.

### Implemented M1.3 File Artifact transfer

Schema version 2 migrates version 1 in one SQLite transaction without discarding profiles or cached declarations. It records File jobs with association identity, expected size, strong response ETag, accepted byte count, SHA-256 checkpoint, attempts, timestamps and last error. Job locks prevent two local processes from transferring the same job; the SQLite active-job index prevents duplicate active jobs for one profile/association. `Queued`, `Downloading`, `Verifying`, `Committing`, `Completed`, `Paused`, `Failed` and `Cancelled` are durable states. Cancelled staging is retained; there is no automatic content deletion. A CLI invocation reconciles interrupted active states and verifies any interrupted commit before marking it complete.

Staging uses `staging/<job-id>/file.part`. File bytes and the parent directory are synchronized before SQLite records a checkpoint. On resume, the staged prefix is rehashed against the stored checkpoint; an unrecorded tail is truncated and a corrupt prefix restarts from zero. A strong ETag is required. The client validates exact `Content-Length`, `Content-Range`, offset and total for `206`; a full `200` restarts the file, and a valid `416` resets and retries within the bounded retry policy. Transport failures and HTTP 429/502/503/504 receive at most three short retries; digest mismatch and authorization errors do not. The content root's available space is checked before transfer. Only regular files are accepted; on Unix file opens refuse final-component symlinks.

Verified bytes are linked without whole-file copying into `objects/<digest-prefix>/<digest>` and `artifacts/<profile-id>/<association-id>/<digest>` on the same content filesystem. The linked inode is made read-only before completion; existing objects and materializations are rehashed before reuse. The content-root and SQLite database may be on separate filesystems; the file hierarchy is synchronized before the SQLite completion transaction. A crash in `Committing` is reconciled by re-verifying staged or promoted bytes. `download files` rehashes completed materializations before reporting them offline. A local user who deliberately changes permissions can still modify their own bytes; subsequent reads detect a digest mismatch. No filename, executable permission, installation or launch behavior is inferred from the bytes. Linux durability has been exercised; Windows/macOS directory-sync behavior needs platform validation.

### Implemented M1.4 Tree Artifact transfer

Schema version 3 migrates version 1 or 2 transactionally, retaining File jobs and verified content. A Tree job stores the complete inventory JSON and strong inventory ETag after validating every page, canonical order, Tree Identity v1 and destination names. `tree_files` stores file ID, original path, size, digest, executable flag, checkpoint, transfer ETag and verified state for each member. An interrupted inventory fetch leaves no partial inventory and is retried from the first page. The job's accepted progress is the sum of durable member checkpoints, advanced by checked per-member deltas in one transaction with the member record. Each member stages at `staging/<job-id>/files/<file-id>.part`, resumes through the shared bounded receiver, and promotes to the immutable object store after SHA-256 verification. For a member of at most 64 KiB, the final staging checkpoint can be omitted: its bytes and the object directory are synchronized before the transaction marks the whole member verified. A crash before that transaction can require re-downloading the member. A verified member is rechecked before reuse after restart. Tree members do not create standalone File Artifact associations.

`staging/<job-id>/tree.build` holds the reconstructed tree; `tree.ready` is its synced, verified form. On Unix, member paths are created relative to opened directory handles with no symlink following. Linux uses no-replace directory promotion into `artifacts/<profile-id>/<association-id>/<tree-digest>`. File bytes are copied into the visible tree so its executable flag can differ from other uses of the same immutable object; only the user execute bit is set when declared. A completed tree is rescanned and each member rehashed on offline listing. A crash in `Committing` is reconciled from the ready tree, promoted destination or verified member objects. A failed database completion leaves the job uncompleted for reconciliation. Windows/macOS lack an implemented atomic no-replace Tree promotion and cannot complete Tree jobs yet; their path and directory-sync behavior still needs platform testing.

NativePortableV1 uses the version 4 `installations` table for a locally assigned runtime identity, preparation/removal state and last spawned process identity. A completed profile/Association/Artifact reference and a cached Release declaration snapshot are required. Archived profiles retain these references for offline use. Preparation independently copies verified object bytes into `apps/<local-id>/`; Linux tries FICLONE reflinks and falls back to ordinary copies, never writable hard links. The desktop starts preparation automatically after a compatible download; direct CLI downloads and older verified downloads prepare transparently on first Play. The selected target and declared working directory are opened without following symlinks before direct execution; Play does not hash installed files. User modifications inside the writable installation are permitted and do not alter immutable source identity. `runtime verify` compares installed files with published inventory and reports missing, modified and unexpected paths without changing anything. Confirmed `runtime repair` retains the old installation in `staging/runtime-quarantine-<local-id>` and restores from verified objects, keeping managed user data. A pre-existing repair backup must be inspected before another repair; the client will not overwrite it. App Depot-owned `userdata/<local-id>/` is separate, but native programs without sandboxing may also write outside the library; such external files are never deleted heuristically.

Version 5 adds `queue_rank` and `auto_resume` to durable download jobs. The desktop schedules one application-level transfer at a time for the whole library, regardless of profile. A new explicit request receives the highest pending rank; another active transfer pauses at its next durable checkpoint (up to the configured checkpoint interval for a File member). Preempted jobs retain their ID, staged bytes and verified Tree members, and return to the queue after the transfer slot is released. A user Pause clears automatic resumption intent. `download move-top` changes pending priority; `download enqueue` records a request for the desktop scheduler. Direct CLI `download start` uses the same process-wide file lock and leaves a job queued when another transfer owns the slot. Restart reconciliation uses persisted state, not live telemetry. SQLite WAL connections have a five-second busy timeout; schema initialization is serialized, and network/body transfer, hashing and filesystem copy occur outside write transactions. A completed catalog sync marks unseen cached records stale. The connected Browse view hides stale entries, while offline cached browsing and local verified Library entries remain available.

On the CachyOS Btrfs development filesystem, a disposable 16 MiB random-file experiment gave a 16 MiB shared extent after `cp --reflink=always`, while an independently copied and synchronized file had 16 MiB exclusive extent according to `btrfs filesystem du -s`. Reflink creation took about 0.002 s and ordinary copy about 0.004 s in this warm local experiment. These are filesystem measurements, not a whole-game benchmark or a guarantee of zero additional space; the client still validates copies and falls back to independent bytes when FICLONE is unavailable.

## Job state and atomic completion

Durable states: `Queued`, `FetchingInventory`, `Downloading`, `Verifying`, `Committing`, `Completed`, `Paused`, `Failed`, `Cancelled`. Record requested profile, Artifact ID, association context, expected size/digest, file IDs and paths, inventory ETag, per-file validated byte ranges, transfer ETags, attempts, last problem, and final location. Persist state before network work. The desktop displays transient bytes written to staging as received progress and exposes the durable accepted-byte checkpoint in Technical details. Live observations are discarded on restart; only durable checkpoints and verified object records participate in recovery. A live progress bar never proves completion.

For each file, write to a unique staging file with bounded chunks and fsync its data before advancing a durable range checkpoint. On resume, recheck staged length and rehash the whole staged file before trusting completed state; a journal is an optimization, not integrity evidence. Request missing bytes with `Range` and strong `If-Range`; accept 206 only when `Content-Range`, length and validator match the requested segment. If the Server replies 200, discard/restart that staging file from byte zero and verify the full body. Treat 416 by revalidating local length and remote metadata, then restart or fail clearly. No cross-origin credential redirects.

For a tree, fetch all inventory pages without filters, ensure ETag and identity are stable, validate canonical paths and destination representability, check every file hash/size and recompute Tree Identity v1 from the full inventory. Materialize only regular files and explicit directories; set only the portable executable flag where supported. A destination with case or Unicode collisions fails before writing. Do not follow symlinks in destination or staging. The single-file Artifact ID itself gives the expected SHA-256. A completed tree has verified member files and the canonical tree digest.

Commit by preparing a complete verified object/tree under the content root, syncing files and directories, then atomically promoting it without replacing an existing destination on the same filesystem. File Artifacts use no-replace hard links; Tree Artifacts use Linux no-replace directory rename. Update the SQLite record in a transaction after promotion; on crash, recovery reconciles either an orphaned completed path or a job still in `Committing` by re-verifying it. Never mark Completed before the destination exists durably. Do not silently replace an existing completed version. Keep failed staging for bounded retry or explicit cleanup, with disk quota and free-space checks. A paused/failed job does not turn into a verified library item.

## Deletion and limits

Client removal is explicit and local. `runtime remove` removes one writable copy while keeping its verified download and managed data; `runtime remove-complete` or `runtime remove-artifact` also removes that Association's local download, jobs, managed data and unreferenced objects. `runtime remove-app` removes the selected App's writable copies; `runtime remove-app-complete` processes all its cached Associations. The profile, credentials and Server resources remain. A durable `Removing` row resumes deletion after interruption. Linux deletion uses directory handles, no-follow traversal and mount-ID checks; an active tracked process or detected runtime process blocks removal. SQLite references and completed jobs are removed only after owned filesystem entries have been reconciled. Immutable object cleanup checks all verified references, download jobs and Tree member records under a write transaction. Shared objects are retained. Logical usage is available through `library usage`; reflinks, filesystem compression and hard links make exact physical reclaim per item unavailable.

Decky complete uninstall confirms an App and profile snapshot, then revalidates and journals all selected Associations under the lifecycle and download locks before deleting managed data. Transfer queue guards prevent new work for Associations with pending complete removal. Library recovery reuses the same durable installation records. The existing schema version remains readable by prior clients. Complete uninstall includes data and modifications inside App Depot's managed directories. User files outside those directories remain outside the deletion scope.

Each verified File/Tree is inspected independently for offline presentation. A damaged legacy Tree with an extra runtime-created file produces an item-level issue; it does not hide other library entries. Explicit `runtime repair-source` rebuilds a Tree materialization from verified local member objects and preserves the previous directory in `staging/source-quarantine-<job-id>`. A complete removal of that Association also removes its quarantine. The writable runtime can be repaired from immutable objects without network access after its current files are removed with confirmation. No runtime changes are promoted into immutable objects.

`library migrate DESTINATION_ROOT --confirm` copies a selected marked library, including separate metadata/content roots, into a new single root. The source remains intact. Current client instances hold a shared library access lock; migration takes it exclusively and refuses an open source library. SQLite is copied through `VACUUM INTO`, not by moving an open database; copied files are rechecked, and a temporary destination is promoted without replacement after opening it successfully. Unknown regular files inside an old materialization are preserved but do not become trusted. A failed or interrupted migration leaves the old root usable and may leave a `.appdepot-migrating-<UUID>` staging directory for manual inspection; it never auto-deletes the source. Close other client instances and pause active transfers before migration. Older client versions do not honor the new access lock. Cleanup of an old source root is deliberately not automated because unrelated files and post-migration changes cannot yet be proven absent. Tokens remain in native secure storage and are not copied into the library.

Schema version 6 adds the profile-scoped `installation_plans` table in one
transaction. It stores a local UUID, the resolved JSON (including the exact
Release revision, platform, normalized selections and source references), and a
SHA-256 fingerprint of the locally serialized JSON. This fingerprint is not a
Server identity. No installation state or verified content is changed by saving
a plan. Clients limited to schema 5 reject the newer library; use a pre-upgrade
backup if returning to an older client. The reader never executes plan metadata.

### Standard installation materialization boundary

The shared `materialization::destination_layout(&Plan)` projects a previously
validated Standard plan to destination `InventoryEntry` values. These are not
a Tree Identity inventory: source file IDs and digests can repeat. Paths are
relative to the payload root, with explicit parent and empty directories.
Core authorizes each source against its completed Artifact reference before
calling the filesystem adapter. The adapter independently checks size and
SHA-256 and copies writable independent files, preserving portable executable
attributes on Linux. The adapter publishes a generation UUID directory under
`apps/` without replacement, with a `payload/` directory and a sibling
`.app-depot-installation-id` marker containing the UUID. Publication does not
activate a generation; only the Core SQLite transaction does. Verification
checks the marker and exact destination layout without mutation. The shared
Core calls `publish_generation(content, id, entries, object)` and
`verify_generation(root, id, entries)` in `materialization`. Windows uses
`windows_fs::publish_tree_install(to, id, entries, object)` and
`windows_fs::verify_tree_install(to, id, entries)`; the object callback maps a
SHA-256 digest to an already authorized CAS path. No adapter may execute
prerequisites or infer launch semantics.

Schema version 7 adds `release_installations`, `installation_generations` and
`installation_sources` in an additive transaction. The stable local installation
identity is keyed by profile and Release, and each new generation references
one saved plan ID. Legacy `installations`, verified downloads, jobs, saved
plans and credentials remain unchanged. Schema version 8 additionally journals
prerequisite consent rounds and outcomes. Older clients refuse schema 8; use a
pre-upgrade backup to return to an older executable.

An explicit Standard install journals a `Preparing` generation and all final
payload source Association/Artifact pairs before acquisition. Existing completed
jobs and verified references are reused; absent sources use the existing File
or Tree jobs and normal authorization. Each saved source is checked against
its completed File identity or Tree inventory and its CAS object is rehashed.
Prerequisite source pairs are retained with the generation but downloads are
acquired and verified only for explicitly selected prerequisites during processing.
Each selected attempt is journaled before source preparation. It receives an
independent verified copy of the entire source Artifact under
`staging/prerequisite-<attempt-id>/<attempt-id>/payload`. Tree paths and empty
directories are preserved, including supporting files absent from the final
application layout. A File Artifact has no declared relative path, so its
single local filename is `prerequisite`. The executor receives the source root,
declared file and containing directory, not a CAS path. Source ownership is
recorded by the existing attempt ID and an exclusive workspace marker; partial
publication stays within that workspace. Definite outcomes permit cleanup;
interrupted or ambiguous attempts retain their source workspace for review.
Progress reports remaining workspaces and retries cleanup of definite
outcomes. A retry always uses a new independent layout. Linux uninstall checks
attempt workspaces for running processes and removes them before deleting their
journal. Windows Standard removal integration must consume the same attempt
IDs and establish process quiescence before removing any retained workspace.
No additional schema migration is needed.
The final map is independently copied into a new
writable `payload/` directory. On Linux its reproducible staging directory is
`staging/manifest-<generation-id>`; retry of the same saved plan discards only
that interrupted staging directory and rebuilds from verified sources. A
published generation is reverified on retry, never overwritten. The SQLite
activation transaction retires the former active generation, sets the new
state and switches the active pointer together after filesystem synchronization.
Any declared prerequisite, including an optional-only list, produces
`PrerequisitesPending` without changing the active pointer. Only completion of
the prerequisite lifecycle activates that generation.

The consent journal binds a generation to its exact saved plan fingerprint and
explicit selected prerequisite IDs; all required IDs must be selected. Each
round is single-use. Outcomes are recorded in manifest order (declined,
running, succeeded, failed, cancelled, ambiguous or verified reuse), with
timestamps, actual exit codes and diagnostics. The saved plan retains expected
exit codes and source identities. Failure or cancellation stops the round;
another attempt requires explicit retry authorization, including review of an
interrupted running attempt (recorded as ambiguous when progress is inspected
without an active coordinator, or when retry is confirmed).
A prior completion may be reused only after the platform executor verifies its
effects in the same generation and the source is revalidated. The final Ready
transition, retirement and active pointer change commit in one SQLite
transaction. An interrupted activation therefore retains the prior active
generation; explicit retry can recover after verifying prior completions.
Schema-7 generations and active pointers are preserved during migration;
previously activated pending generations remain unready, and no historic
consent or prerequisite completion is inferred.

An error retains the previous active generation and the pending journal with
its diagnostic. `install PLAN_ID` resumes that journal; another saved plan
cannot replace an unfinished preparation. Retired generations and their user
changes are preserved, including while a program is running. Each explicit
successful reinstall creates a new generation. `install status RELEASE_ID`
reports the active pointer, all retained generations and errors. `install
verify RELEASE_ID` is read-only and checks the marker, SHA-256, file sizes,
portable executable modes and exact layout of the active generation, independent
of readiness. It never repairs or executes a file.

`install uninstall RELEASE_ID --confirm` journals `Removing` for all owned
generations before deleting their working copies and staging on Linux. Tracked
or detected running programs block the operation. Retrying the same command
reconciles interrupted removal. The saved plans, verified source downloads and
CAS remain. Complete legacy Association/App removal is blocked while a Standard
generation owns a source; uninstall the Standard Release first. The retained
download references continue to protect CAS through normal collection. No data
outside the managed generation directories is removed.

Common installation acquisition, generation state, publication and verification
coordination uses the Windows adapter as well as Linux. Windows Play uses the
active generation UUID as its selection guard and records Job Object supervision
in the generation row before resuming the pinned executable. The watcher
retains occupancy after the Desktop closes and records completion after observed
job descendants exit. Windows Standard CLI installation, verification and
runtime-only removal use the shared generation state. The Windows-specific
`windows_generation_removals` journal references generation IDs and records
NTFS identities through Intent, Quarantined and Cleaning, with startup
reconciliation. Legacy removal tables retain their existing foreign keys.
Removal requires a complete verified owned layout and confirmed idle occupancy.
If the layout is incomplete, modified or contains unrecognized content, the
operation refuses removal and preserves it. Interrupted cleanup retains its
journal and source references until final SQLite deletion. Native prerequisite
execution remains unavailable in the CLI. Windows may leave unpublished staging for
manual inspection, as in its existing Tree installer; the Linux retry cleanup
policy does not apply to those directories.
