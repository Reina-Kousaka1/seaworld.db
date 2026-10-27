# Transactions and recovery

## Current guarantees

The initial engine is append-only. Each mutation is represented by one checksummed operation block, appended in sequence, and followed by `FileChannel.force(true)`. The in-memory state changes only after that call succeeds. On reopen, verified operation blocks are replayed to rebuild collections and the primary-ID map.

An incomplete final block is discarded with an explicit recovery report. A complete block with invalid checksum or invalid semantics fails open. A force failure has an ambiguous outcome: a complete block may exist even though the caller received an error. Callers must close and reopen after an append error before retrying. The current handle rejects further operations after such an error.

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

Crash tests must kill a separate process at controlled write stages. Clean shutdown tests alone are not evidence of crash recovery.
