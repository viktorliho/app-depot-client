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

## Job state and atomic completion

Durable states: `Queued`, `FetchingInventory`, `Downloading`, `Verifying`, `Committing`, `Completed`, `Paused`, `Failed`, `Cancelled`. Record requested profile, Artifact ID, association context, expected size/digest, file IDs and paths, inventory ETag, per-file validated byte ranges, transfer ETags, attempts, last problem, and final location. Persist state before network work. Progress reported to UI is from durable accepted bytes, never merely bytes read from a socket.

For each file, write to a unique staging file with bounded chunks and fsync its data before advancing a durable range checkpoint. On resume, recheck staged length and rehash the whole staged file before trusting completed state; a journal is an optimization, not integrity evidence. Request missing bytes with `Range` and strong `If-Range`; accept 206 only when `Content-Range`, length and validator match the requested segment. If the Server replies 200, discard/restart that staging file from byte zero and verify the full body. Treat 416 by revalidating local length and remote metadata, then restart or fail clearly. No cross-origin credential redirects.

For a tree, fetch all inventory pages without filters, ensure ETag and identity are stable, validate canonical paths and destination representability, check every file hash/size and recompute Tree Identity v1 from the full inventory. Materialize only regular files and explicit directories; set only the portable executable flag where supported. A destination with case or Unicode collisions fails before writing. Do not follow symlinks in destination or staging. The single-file Artifact ID itself gives the expected SHA-256. A completed tree has verified member files and the canonical tree digest.

Commit by preparing a complete verified object/tree under the library root, syncing files and directories, then atomic rename within the same filesystem into an immutable completed location. Update the SQLite record in a transaction after the rename; on crash, recovery reconciles either an orphaned completed path or a job still in `Committing` by re-verifying it. Never mark Completed before the destination exists durably. Do not silently replace an existing completed version. Keep failed staging for bounded retry or explicit cleanup, with disk quota and free-space checks. A paused/failed job does not turn into a verified library item.

## Deletion and limits

Client removal is an explicit local operation, separate from Server deletion. Shared verified objects use local reference counts or a mark-and-sweep pass anchored by completed records; do not delete bytes referenced by another local item. Apply configurable storage/transfer limits and bounded cache eviction. Artwork cache is disposable; completed Artifact bytes and their proof manifest are not. Never put Bearer tokens in SQLite, job files or the download path.
