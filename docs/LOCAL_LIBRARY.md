# Local library and recovery contract

## Layout and identity

Use OS user data directories (`$XDG_DATA_HOME/app-depot-client` or `~/.local/share/app-depot-client` on Linux; Known Folders on Windows; Application Support on macOS). Permit a user-selected library root. Within it:

```text
library.sqlite       local profiles, snapshots, jobs and verified locations
objects/             immutable verified file bytes, keyed by digest
artifacts/           completed user-visible file/tree materializations
staging/<job-id>/    partial bytes, inventory snapshot and resume journal
artwork/             bounded, validated display cache
```

Paths are illustrative local implementation details, not Server protocol fields. Persist `ServerProfileId`, normalized connection URL, display name, capability snapshot, token reference, App/Release/Association IDs, Artifact identity, declaration raw JSON and revision/ETag, optional channel snapshot, artwork metadata, download jobs, completion manifests and timestamps. Store content length and digests as exact 64-bit integers/hex; no JSON floating-point conversion. One profile may be offline, deleted remotely or inaccessible while its completed local copies remain available. Show cached metadata with last-sync time and stale status. Do not claim a local copy is still authorized by the Server.

### Implemented M1.2 foundation

`LibraryPaths` defaults to the OS user data directory plus `app-depot-client`: XDG data on Linux, AppData on Windows, and Application Support on macOS. CLI flags/environment can select the metadata root and content root independently. The metadata root contains `library.sqlite`; the content root contains prepared `objects/`, `artifacts/`, `staging/`, and `artwork/` directories. Both roots carry the same `.app-depot-library-id` UUID marker so a data root cannot silently be paired with another content root. On Unix, newly created directories are private and the database is mode `0600`; existing roots/databases with group or other access are refused rather than silently changed. Root paths must be absolute and contain no symlink components or `.`/`..`. Initialization creates only its named entries and refuses unknown existing SQLite schemas. Windows/macOS filesystem validation still needs platform testing.

Schema version 1 stores profiles, App/Release/association/Artifact metadata JSON, and Release declaration snapshots with scope revision IDs and snapshot ETags. JSON uses exact integer parsing and preserves unknown properties. The schema reserves minimal `verified_content` and `download_jobs` identity/state tables; M1.2 does not write them or mark any content verified. Timestamps are Unix milliseconds. Every successful sync writes fetched metadata and the profile's capabilities/status/timestamp in one SQLite transaction. A failed fetch leaves the prior catalog and last successful sync intact. Cached rows retain their last-seen time; `stale` means absent from the latest completed sync, while `source: cache` always distinguishes them from a live API response. Successful synchronization never deletes unseen rows, since remote publication and access can change. A Release snapshot is internally consistent, but the Server does not expose a globally atomic catalog snapshot.

Changing an origin creates a new profile ID and archives the old profile/cache. Explicit profile removal also archives and retains its metadata for offline browsing, while removing its secure-store token. Archived profiles cannot synchronize. The CLI offers no purge in M1.2. Tokens are stored only in native secure storage or process memory. If Secret Service, Credential Manager or Keychain is unavailable, the client uses anonymous access or a temporary `APP_DEPOT_TOKEN`; it never falls back to plaintext storage. Native storage was validated on the Linux development host; Windows/macOS backends remain untested.

## Job state and atomic completion

Durable states: `Queued`, `FetchingInventory`, `Downloading`, `Verifying`, `Committing`, `Completed`, `Paused`, `Failed`, `Cancelled`. Record requested profile, Artifact ID, association context, expected size/digest, file IDs and paths, inventory ETag, per-file validated byte ranges, transfer ETags, attempts, last problem, and final location. Persist state before network work. Progress reported to UI is from durable accepted bytes, never merely bytes read from a socket.

For each file, write to a unique staging file with bounded chunks and fsync its data before advancing a durable range checkpoint. On resume, recheck staged length and rehash the whole staged file before trusting completed state; a journal is an optimization, not integrity evidence. Request missing bytes with `Range` and strong `If-Range`; accept 206 only when `Content-Range`, length and validator match the requested segment. If the Server replies 200, discard/restart that staging file from byte zero and verify the full body. Treat 416 by revalidating local length and remote metadata, then restart or fail clearly. No cross-origin credential redirects.

For a tree, fetch all inventory pages without filters, ensure ETag and identity are stable, validate canonical paths and destination representability, check every file hash/size and recompute Tree Identity v1 from the full inventory. Materialize only regular files and explicit directories; set only the portable executable flag where supported. A destination with case or Unicode collisions fails before writing. Do not follow symlinks in destination or staging. The single-file Artifact ID itself gives the expected SHA-256. A completed tree has verified member files and the canonical tree digest.

Commit by preparing a complete verified object/tree under the library root, syncing files and directories, then atomic rename within the same filesystem into an immutable completed location. Update the SQLite record in a transaction after the rename; on crash, recovery reconciles either an orphaned completed path or a job still in `Committing` by re-verifying it. Never mark Completed before the destination exists durably. Do not silently replace an existing completed version. Keep failed staging for bounded retry or explicit cleanup, with disk quota and free-space checks. A paused/failed job does not turn into a verified library item.

## Deletion and limits

Client removal is an explicit local operation, separate from Server deletion. Shared verified objects use local reference counts or a mark-and-sweep pass anchored by completed records; do not delete bytes referenced by another local item. Apply configurable storage/transfer limits and bounded cache eviction. Artwork cache is disposable; completed Artifact bytes and their proof manifest are not. Never put Bearer tokens in SQLite, job files or the download path.
