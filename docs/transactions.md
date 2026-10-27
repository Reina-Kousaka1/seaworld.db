# Transactions and recovery

## Current guarantees

The Rust engine is append-only. Each mutation is represented by one checksummed operation block, appended in sequence, and followed by `File::sync_all()`. The in-memory state changes only after that call succeeds. On reopen, verified operation blocks are replayed to rebuild collections and the primary-ID map. Subprocess tests terminate a writer at controlled append and sync points; these tests exercise process-crash recovery, not machine power-loss behavior.

An incomplete final block is discarded with an explicit recovery report. A complete block with invalid checksum or invalid semantics fails open. A `sync_all` failure has an ambiguous outcome: a complete block may exist even though the caller received an error. Callers must close and reopen after an append error before retrying. The current handle rejects further operations after such an error.

## Not provided yet

The first version does not provide multi-operation transactions, rollback, snapshot isolation, serializable isolation, MVCC, concurrent writers, checkpoints, or a separate WAL. It does not claim full ACID. A single operation block is a replay unit, not a complete transaction protocol.

The append-only record stream is the primary storage log, so it serves as a simple journal. Future page-based storage should use a real WAL or another design with equivalent write-ahead ordering, commit markers, checksummed records, checkpointing, and bounded recovery.

## Planned transaction design questions

Before claiming transaction guarantees, specify and test:

1. transaction IDs and begin/commit/abort markers;
2. which mutations are durable before a commit marker;
3. rollback behavior in memory and on disk;
4. isolation and lock ownership;
5. recovery of incomplete and committed transactions;
6. idempotent repeated recovery;
7. force ordering and directory-entry durability for create/rename/checkpoint operations;
8. crash-injection points around write, force, commit, checkpoint, and reopen.

The current subprocess harness covers process exit before append, during a partial block, after a full write before sync, and after sync before the operation returns. It does not simulate power loss, storage-controller cache loss, or all filesystem failure modes.
