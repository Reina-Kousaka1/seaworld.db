# Core invariants and durability review

This document records the local SeaWorld-core contract. It separates guarantees
that are implemented and tested from mechanisms that were reviewed as future
architecture inputs. SeaWorld remains a one-file, embedded Rust engine; this is
not a commitment to adopt another database's storage engine or protocol.

## Current invariant ledger

| Invariant | Current SeaWorld mechanism and evidence |
| --- | --- |
| Acknowledged commit boundary | V2--V4 append a complete checksummed transaction batch, call `File::sync_all()`, then publish in-memory state. A successful `commit()` returns only after that sync. |
| Atomic crash visibility | Replay applies a batch only after its matching `TX_COMMIT`. Process-kill tests require the complete before or after state, never a partial batch. |
| Unacknowledged writes | A suffix without a complete matching commit is truncated only by writable recovery; read-only opens return `RecoveryRequired`. |
| Confirmed data on recovery | Complete, valid committed batches replay in log order. Complete checksum or semantic failures fail closed instead of being skipped. |
| Metadata/data coherence | Transaction replay validates begin/mutation/root/commit order, identifiers, watermarks, collection state, and index references. |
| Index truth | V4 pages and root publications are in the same batch as document changes. Reopen and integrity checks validate tree contents against live documents; executor candidates are filter-rechecked. |
| Compaction/backup safety | A same-directory temporary copy is validated before no-overwrite hard-link publication. Source data is unchanged; V4 compaction retains V4 and rebuilds live persistent roots. |
| Format compatibility | V1--V4 remain readable; the Java V1 fixture is an integration fixture. New files use V4. |
| Semantics-neutral diagnostics | Optimization observation is opt-in, bounded, non-persistent, and recommendation-only. |
| Writer concurrency | One cooperating writer is enforced by a sidecar lock and Rust mutable ownership. This prevents overlapping writer commits; it is not multi-writer MVCC. |

The file-sync result is the API acknowledgement boundary, not a universal
power-loss promise. Filesystem, operating-system, controller-cache, and device
behavior remain outside the process-crash qualification. A failed sync is
outcome-ambiguous and the handle fails closed until reopen.

## Commit and recovery boundary

```text
validate bounded staged state and index batch
  -> append TX_BEGIN / mutations / V4 pages / V4 roots / TX_COMMIT
  -> File::sync_all()                 (acknowledgement boundary)
  -> publish documents, MVCC, indexes (in-memory materialization)
```

The commit marker is the recovery decision point. The synchronised file is the
durable SeaWorld input for a later reopen. In-memory publication is deliberately
after sync: it cannot make an uncommitted batch visible. Internal post-sync
invariant failures poison the handle rather than continuing from a potentially
inconsistent in-memory view; reopen replays the authoritative log.

## Fault-injection qualification

The process harness covers transaction begin, operations, V4 page/root
publication, commit write, sync, and post-publication boundaries. It also now
covers both generic V4 compaction and backup at temporary creation, copy
transaction, pre-publication, post-hard-link/pre-directory-sync, and
post-directory-sync/pre-cleanup boundaries. A pre-publication kill exposes no
destination; a post-publication kill may expose only a fully validated copy.

V1 tail recovery is injected after truncation before sync and after sync. The
next open must be coherent and idempotent. These are process-termination tests,
not power-failure simulation.

## Reviewed mechanisms and decisions

| Reference mechanism | SeaWorld conclusion |
| --- | --- |
| [FoundationDB conflict ranges and commit versions](https://apple.github.io/foundationdb/developer-guide.html) | SeaWorld already has ordered local commit versions and a single durable commit point. Read/write conflict sets and client retry become relevant only after a separately specified multi-writer model; do not imitate them under the one-writer contract. |
| [WiredTiger checkpoints and logging](https://source.wiredtiger.com/develop/arch-checkpoint.html) | The useful principle is a consistent checkpoint plus replayable log, with an explicit publication boundary. SeaWorld's compacted copy is its current validated materialization path. A mutable in-place checkpoint generation system or WiredTiger integration needs a separate migration and failure-model decision. |
| [RocksDB WAL/manifest tracking](https://github.com/facebook/rocksdb/wiki/Track-WAL-in-MANIFEST) | Metadata must not claim a log is durable or obsolete unless recovery can verify it. SeaWorld keeps document, page, root, and commit metadata in one checksummed append log instead of adding a second manifest. |
| [ScyllaDB shard-per-core ownership](https://docs.scylladb.com/manual/stable/kb/gossip.html) | Local ownership and bounded admission are useful future server principles. Shard-per-core is not appropriate to add before a local multi-writer and async ownership model exist. |
| [CockroachDB MVCC/range/replication layers](https://www.cockroachlabs.com/docs/stable/developer-basics.html) | Stable local MVCC and a clear storage boundary are worthwhile precursors. Ranges, Raft, replication, and serializable retry semantics are distributed-system work and are excluded. |
| [SingleStore logical versus physical layouts](https://docs.singlestore.com/db/v8.5/create-a-database/choosing-a-table-storage-type/) | Logical document semantics must remain independent from future physical representations. SeaWorld currently has documents plus indexes only; analytical materializations are not introduced. |

## Deliberately deferred decisions

- Multi-writer conflict detection, automatic retries, field/path conflict
  tracking, and commutative concurrent mutation semantics.
- In-place checkpoints, persistent checkpoint generations, and a separate
  manifest. These need an explicit on-disk migration and power-loss test plan.
- Admission control, disk-pressure backpressure, async scheduling, and
  shard-per-core ownership. Existing resource limits fail operations before
  append; they are not a load-control subsystem.
- Replication, Raft, ranges, cluster membership, and distributed transactions.
- Additional physical representations, automatic storage plans, or automatic
  optimization application.

Before any such change, define the durable authority, publication order,
recovery oracle, resource bounds, and fault-injection matrix first. No new
format or public-API guarantee follows from this review.
