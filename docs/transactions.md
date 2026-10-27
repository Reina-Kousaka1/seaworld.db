# Transactions and recovery

## V1 compatibility and V2/V3/V4 selection

V1 remains byte-for-byte unchanged. The reader continues to replay its four operation records, and a V1 handle keeps the original single-operation CRUD behavior. `begin_transaction` and secondary-index creation return an explicit upgrade-required error for V1 files. `write_v2_compacted_copy`, `write_v3_compacted_copy`, and `write_v4_compacted_copy` are non-destructive migration paths. New databases use format V4; explicit V1, V2, and V3 constructors remain available for compatibility tooling.

V2, V3, and V4 use the existing append-only file as their transaction log. They do not add a second WAL. V4 stores immutable B+Tree page images as checksummed records in the same file. The versioned header distinguishes the record grammar, so a V1 reader never encounters later transaction markers in a file labeled V1.

## Commit protocol

The V2/V3/V4 log writes one contiguous transaction batch:

1. `TX_BEGIN(transaction_id)`;
2. zero or more checksummed mutation records tagged with that ID;
3. in V4, copy-on-write index pages and root-publication records;
4. `TX_COMMIT(transaction_id)` as the final record.

The engine writes the complete batch, synchronizes the file with `File::sync_all()`, then publishes the staged changes to the in-memory state and returns success. The commit marker is the recovery decision point; the sync is the API's success boundary. A sync error is outcome-ambiguous, so the handle fails closed and must be reopened. A complete marker may replay even if the caller observed an error.

`Transaction` holds the database's exclusive mutable borrow. Its own reads see staged changes; no other handle can write the file, and ordinary reads through the borrowed `Database` are unavailable until commit or rollback. V3/V4 provide owning MVCC snapshots and fixed read-only handles for concurrent readers; the database still permits only one writer. Rollback discards its in-memory operation list and writes nothing. IDs returned by an uncommitted insert are provisional and can be reused after rollback; only committed IDs are durable.

Transaction IDs are positive, monotonically allocated by committed transactions and persisted in log order. In V3, the public `CommitVersion` is a distinct type with the same numeric ordinal during normal operation. A rolled-back transaction writes nothing, so the next transaction may reuse its provisional ID. Overflow is an error. The log retains only the current pending transaction during replay; V2/V3 bound transactions to 10,000 operations and 64 MiB of encoded operation data.

V3 adds in-process MVCC snapshots, bounded to 1,024 leases and 64 MiB of retained encoded document history. V4 retains those semantics. Read-only handles replay under a shared commit lock and then use a fixed in-memory view while one writer appends. An incomplete suffix discovered in read-only mode returns `RecoveryRequired`; only a writable open may truncate it. V3/V4 compaction refuses while snapshots remain active. MVCC history is rebuilt from the current committed state on reopen and is not retained across process restarts.

V4 page records and root publications are staged with ordinary document operations. Replay validates the B+Tree pages and applies the root only after seeing the matching commit marker. A crash after page writes but before the commit marker discards the entire transaction from its begin record, leaving the previous root visible. This is process-crash behavior; it does not establish power-loss durability.

## Recovery state machine

Replay applies only complete committed transactions. It validates transaction ID ordering, begin/operation/commit ordering, each record sequence and checksum, and operation semantics. A valid but uncommitted transaction at EOF is discarded from its begin offset. A torn final record also discards the whole containing transaction. Recovery reports all discarded bytes and syncs the truncation. A complete record with a bad checksum or invalid transaction state fails open; recovery never skips it. Reopening after recovery is idempotent.

The process crash harness covers before begin, after begin, a partial first operation, between operations, after the last operation, partial commit markers, V4 page/root-publication boundaries, after commit write before sync, after sync, and interruption during recovery. Process-kill tests do not simulate power loss or device-cache loss.

## Durability limits

`commit()` reports success only after `sync_all()` succeeds. That asks the operating system to synchronize file contents and metadata; filesystem, storage hardware, and OS behavior still determine power-loss durability. Linux and Windows CI verify process-level behavior, not power-failure guarantees. No full ACID or production durability claim is made.
