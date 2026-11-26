# Backups and restore

`Database::backup` writes a compacted V4 copy of the handle's committed view to a new path. It checks the source view, writes and synchronizes a temporary sibling, reopens that file read-only, and checks its structure and indexes before publishing it with a no-overwrite hard link. The source is not changed. A writer handle can create a backup between transactions; a fixed `Database::open_reader` handle can capture a committed view while a writer is active. Staged changes are invisible to that reader. V3/V4 backup through a handle with active snapshot leases is rejected with `ActiveSnapshots`.

`Database::verify_backup(path)` opens a fixed read-only view and checks the header, supported version and flags, log records and checksums, transaction state, documents, index definitions, and V4 pages and roots. It returns an `IntegrityReport`. An incomplete tail returns `RecoveryRequired`; verification does not repair it. The `seaworld verify BACKUP.seaworld.db` command performs the same read-only checks. Verification describes the file at the time it was opened under SeaWorld's cooperating file lock; it cannot prevent a later or external modification.

`Database::restore_backup(source, target)` validates the source read-only, then writes, validates, and publishes a new V4 copy. The target must not exist. An invalid backup or a failed write cannot overwrite an existing target; no in-place replacement is offered. A failure after hard-link publication can leave a complete validated target even if a later directory sync reports an error, so callers should inspect the target before retrying. The filesystem must support same-directory hard links. A process killed before publication can leave temporary sibling files that require manual cleanup.

V1, V2, V3, and V4 files are accepted as sources when their existing readers validate them; backup and restore output V4. The database UUID, live collection and document IDs, index definitions, and committed document-ID high-water mark are preserved. Compaction may advance the commit-version watermark when reconstructing the copy, but never moves it backwards. A backup records the high-water mark at its own committed view: IDs allocated later in another copy are not known to it, so restored and original databases can diverge while sharing the same UUID. Rolled-back provisional IDs remain reusable under the transaction rules.

Copies and replay obey the existing 128 MiB database, 17 MiB record, transaction, document, index, and snapshot limits. The embedded engine holds its bounded logical state and index entries in memory; backup is not a streaming-only implementation. SeaWorld does not implement backup generations or retention. Files are synchronized before publication and the parent directory is synchronized where supported. Process-kill coverage includes copy creation, copy transaction, pre-publication, and post-publication boundaries for both backup and generic V4 compaction. A process kill before publication exposes no destination; after hard-link publication it can expose only the already validated destination. Windows, filesystem, OS, and device behavior limit durability. Process-kill tests do not prove power-loss survival. CRC32C detects accidental corruption and is not authentication.

## File capacity and verified compaction

V4 index updates append copy-on-write B+Tree pages and never reuse obsolete
pages, so an indexed write workload grows the file faster than its live data
(about 16.7 KiB per single-document update with one index in the capacity
regression test). When a write would grow the file past `MAX_DATABASE_BYTES`
(128 MiB), it is rejected *before any byte is appended* with
`Error::CapacityExhausted { file_bytes, requested_bytes, limit_bytes }`.
`Error::is_capacity_exhausted()` identifies it; its stable code remains
`resource_limit` for Public API 1.0.0 compatibility. The rejection is never an
I/O, corruption, or integrity error, does not poison the handle, and is not
retried automatically. Committed data stays readable and smaller writes that
still fit are accepted. Retrying the same mutation cannot succeed until the
application switches to a compacted file.

`Database::storage_usage()` reports the exact physical `file_bytes`,
`limit_bytes` and `remaining_bytes`, the stored and reachable B+Tree page
counts, and a labelled *estimate* of live data (`estimated_live_bytes`,
`estimated_reclaimable_bytes`) derived from in-memory documents, collections,
index definitions, reachable pages and record framing. The estimate is not a
promise about the compacted size. `compaction_recommended` is true at
`COMPACTION_RECOMMENDED_FILL_PERCENT` (75 %) of the limit, or from
`COMPACTION_RECOMMENDED_MIN_FILE_BYTES` (8 MiB) when at least half the file is
estimated reclaimable. The call walks all documents and index trees, so it is
meant for periodic checks. `seaworld usage DATABASE` prints the same report.

`Database::compact_verified(target)` is the supported application workflow:

1. write a same-version compacted copy to a synchronized same-directory
   temporary file and validate it read-only;
2. publish it with a no-overwrite hard link and synchronize the directory where
   supported;
3. reopen the published path independently through a read-only handle;
4. run `check_integrity` and compare format version, database ID, collections,
   every live document, index definitions and entries, the document-ID
   watermark and (V3/V4) the commit-version watermark with the source handle;
5. return a `CompactionReport` with the verified absolute path, sizes and the
   copy's `IntegrityReport`.

The source file is never modified, replaced or deleted, and the open handle is
not switched. The application then closes its handle and opens
`report.path` explicitly; retiring the old file is the application's decision.
If verification fails, an `Integrity` (or I/O) error is returned and no report
is produced; a published target is left for inspection and must not be used.
`seaworld compact SOURCE NEW_TARGET` runs the same workflow from a read-only
source handle. Automatic live file switching and page reuse are not
implemented and need a separate architecture decision.

Process-kill coverage for the verified workflow includes copy creation, the
copy's document batch, before and after the copy's commit sync, pre-publication,
post-publication before and after the directory sync, and after publication
before verification. Test-only fault injection (`crash-test` feature,
`SEAWORLD_FAIL_AT`) covers a failed directory sync and a failed reopen of the
copy. Concurrent compactions to one target publish exactly one copy; stale or
corrupt temporary siblings are neither used nor published.
