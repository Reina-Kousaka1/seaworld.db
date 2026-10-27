# MVCC snapshots and reader/writer model

## Scope and format version

V3 introduced the durable commit-version watermark needed to prevent version rollback through compaction; V4 keeps that behavior and adds persistent B+Tree pages. New files use V4. V1 and V2 keep their existing record grammar and CRUD/transaction behavior. A compacted copy may advance the watermark if rebuilding its snapshot requires more transactions than the source log. `Database::create_v2`, `Database::create_v3`, and migration APIs remain available for compatibility tests and tooling. Snapshots require V3 or later.

## Commit versions

`CommitVersion` is a distinct public type from document, collection, index, and transaction IDs. For V3/V4, its positive numeric value is the transaction ID of the last successfully committed transaction. The transaction ID is assigned provisionally at begin, becomes a commit version only after the commit marker has been synchronized, and is reused after rollback or recovery of an incomplete transaction. No wall-clock time participates in version ordering. The version-watermark record preserves the next commit version when a compacted copy rewrites the log.

An empty V3/V4 database begins at version zero. A committed transaction advances the version by one. Standalone index-definition records do not change the document snapshot. Reaching the integer limit prevents another transaction before any bytes are appended.

## Visibility and lifecycle

A snapshot records the current commit version and is an owning, cloneable handle. It does not borrow `Database`, so a writer can commit while an older snapshot remains in use. Reads resolve each document's newest version whose begin version is no greater than the snapshot version. A document update appends a new in-memory version; delete appends a tombstone. A deleted document is absent to snapshots at or after the delete version, while older snapshots retain the last live value.

Snapshots created by a live writer share one bounded version store. Readers hold its read lock only while resolving/querying; a writer publishes a complete commit under one write lock after the file sync succeeds. A query therefore sees a complete old or new version. Snapshot clones share one RAII lease; the active registration is removed when the final clone is dropped.

At open, no snapshots from a previous process can remain. Replay reconstructs the committed logical state and current commit version, then starts a new in-memory version store at that version. The append log remains the recovery source. Old in-memory versions are retained only while an active snapshot may need them, then pruned to the predecessor of the oldest snapshot and later versions. At most 1,024 active snapshot leases and 64 MiB of retained encoded document history are permitted. A write that would exceed the history bound fails before it appends. Rollback creates no version and publishes no state.

Compaction refuses to run while a snapshot lease is active. It writes only the latest committed state, preserves the current version watermark, and leaves the source file untouched. This avoids invalidating a reader's retained versions.

## Concurrent readers and writers

The supported model is one writer handle and any bounded number of readers. A writer owns an exclusive `<database>.writer.lock` lock for its lifetime. Each append/recovery operation takes an exclusive OS lock on the database file; `Database::open_reader` takes a shared lock on that file while it replays a fixed snapshot, then releases the lock and reads its in-memory view. It may remain open while the writer appends later commits. A second writer receives the typed `Locked` error. Read-only handles reject every write API.

Snapshots from the live writer are independently shareable across threads. The reader API never exposes mutable internal state. OS locks coordinate cooperating SeaWorld processes only; applications must not modify an open database file directly. Lock semantics remain subject to platform and filesystem behavior.

## Recovery

V3/V4 use the existing checksummed append log. Recovery applies a transaction's documents and commit version only after its matching commit marker. V3 secondary index entries are rebuilt from committed documents on open. V4 loads entries from the committed persistent B+Tree roots and checks them against live documents. An incomplete transaction suffix is discarded from its begin record. A read-only open never truncates the file; if it finds an incomplete tail, it returns `RecoveryRequired` and asks the caller to open the database for recovery. A complete checksum or semantic error remains a corruption error.

Process-kill tests exercise commit visibility and reopen behavior. They do not establish power-loss durability. `sync_all()` is the API success boundary; filesystem, OS, and device guarantees still apply.

## Deliberate limits

There is no MVCC for V1/V2 handles, no concurrent writer, no MVCC history retained across process restart, and no online compaction while snapshots are active. Snapshot queries initially use a correct full scan over their versioned view; the current-state query planner can use ordered secondary indexes.
