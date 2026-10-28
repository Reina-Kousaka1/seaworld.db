# WiredTiger integration assessment

Status: research and proposed boundaries, not an approved backend or migration.
SeaWorld's Rust engine and V1–V4 readers remain authoritative. WiredTiger is a
possible integrated storage backend, not a backup service. This assessment uses
the upstream development documentation (labelled 12.0.0 on several pages); its
architecture guide warns that it may not match a particular release. Pin and
recheck an actual release before any implementation.

## Existing and candidate responsibilities

Today SeaWorld owns the document model, queries, single-writer transactions,
in-process snapshot history, V4 copy-on-write index pages, append log, recovery,
and verified non-overwriting backups. It stores a single bounded SeaWorld file.
WiredTiger is a C storage engine: a connection owns a database home directory,
sessions provide thread and transaction contexts, and cursors read and update
tables. Its row and column stores use B+Trees. A WiredTiger database home and a
SeaWorld file are different storage formats; neither can be assumed to read the
other. See the [API overview](https://source.wiredtiger.com/develop/basic_api.html)
and [file formats](https://source.wiredtiger.com/develop/file_formats.html).

The following is a proposed allocation **if** a separate WiredTiger backend is
approved; it is not a decision to replace the current engine.

| Function | SeaWorld | WiredTiger | Adapter / shared boundary |
| --- | --- | --- | --- |
| Document model | Types, paths, IDs, validation and limits | Opaque keys and values | Versioned encoding; enforce limits |
| Query engine | AST, planner, predicates, recheck and ordering | Cursor primitives | Map ordered ranges and copy cursor values |
| Transactions | Public contract, constraints and error categories | Atomic storage transaction | Group document, index and metadata writes; map conflicts |
| MVCC | Snapshot contract and lease limits | Transaction snapshots and history store | Match lifetime, isolation and failure semantics |
| Indexing | Definitions, key semantics, uniqueness and planning | Candidate physical B+Trees | Atomic index maintenance and ordering compatibility |
| Cache | Public limits and diagnostics | Page cache and eviction | Configuration and resource accounting |
| WAL/logging | Durability contract | Configured transaction log | Select and test sync policy explicitly |
| Recovery | Compatibility, validation and error reporting | Its own checkpoint/log recovery | Startup sequencing and consistency checks |
| Backup | Identity, verification, no-overwrite restore contract | Backup cursor and relevant files | Produce a consistent, verified backend-specific artifact |
| Compaction | Admin policy and snapshot contract | Table compaction | Map errors and snapshot restrictions |
| Compatibility | V1–V4 readers, explicit migration and public API | Its own format compatibility | Versioned backend identity and recoverable migration |

An alternative is to retain SeaWorld as the only backend. Another is a
side-by-side backend selected explicitly for newly created databases. A proposal
to put WiredTiger underneath the existing SeaWorld append log must explain
which system owns each commit and recovery boundary: two independent logs and
snapshot histories cannot provide atomicity merely by being stacked. None of
these alternatives has been selected. Keep SeaWorld's existing backend and tests
until an owner decision and equivalent compatibility evidence exist.

## Operational differences to prove

- WiredTiger sessions and cursors have ownership and thread constraints; cursor
  data lifetime must not escape its valid scope. Its C API would need a reviewed
  Rust FFI boundary or a well-maintained binding. No binding or native library is
  added here, and no official Rust binding has been established by this review.
  The current crate forbids `unsafe`; any FFI approach requires an explicit
  safety and dependency decision.
- WiredTiger supports concurrent writers and `WT_ROLLBACK` on conflicts or
  resource pressure, including read failures. Its documentation says snapshot
  isolation may still allow phantoms; SeaWorld must independently verify its
  repeated-predicate contract rather than infer equivalence from a label. See
  [transactions](https://source.wiredtiger.com/develop/transactions_api.html).
- The [cache and eviction](https://source.wiredtiger.com/develop/eviction.html)
  manage pages and historical versions; history retention, long snapshots and
  resource pressure differ from SeaWorld's bounded in-memory leases.
- [Checkpoints](https://source.wiredtiger.com/develop/arch-checkpoint.html) and
  [logging](https://source.wiredtiger.com/develop/durability_log.html) are
  separate durability mechanisms. Default commit logging can buffer writes;
  commit durability needs an explicit sync configuration and crash testing.
  Recovery can require the same compression/collation extensions at open.
- [Backup cursors](https://source.wiredtiger.com/develop/arch-backup.html)
  pin a set of metadata, data and log files; blindly copying one live file is
  insufficient. [Compaction](https://source.wiredtiger.com/develop/compact.html)
  operates on WiredTiger objects and may be constrained by checkpoints.
  Compression is configurable and introduces build, recovery and resource
  considerations; no compression or performance claim is made here.
- The upstream source uses CMake and a native C build. Linux ARM64/Raspberry Pi
  cross-compilation, toolchain, native libraries, memory/disk usage, supported OS
  versions, and both Linux and Windows packaging need a reproducible prototype
  and CI before support is claimed. No Pi service or device was changed.

## Licensing and migration gates

The current [upstream license](https://github.com/wiredtiger/wiredtiger/blob/develop/LICENSE)
offers GPL version 2 or 3, or separate commercial terms. GPLv3 section 13
permits combination with AGPLv3 code, with its network-source condition applying
to the combination; constituent licenses and corresponding-source obligations
remain relevant ([GNU guidance](https://www.gnu.org/licenses/gpl-faq.en.html)).
This is an engineering assessment, not a substitute for review of the exact
release, binding, dependencies, distribution and deployment. SeaWorld remains
AGPL-3.0-only; no third-party code is incorporated by this document.

Before activation, decide backend ownership and physical layout; select and
audit a release and binding; prove atomic document/index/metadata transactions,
snapshot and query equivalence, conflict mapping, safe lifetimes, bounds,
recovery and backups on all supported targets. Existing V1–V4 files and the
Java V1 fixture must remain readable. Any migration must be explicit,
non-overwriting, restartable or recoverable, validated before publication and
tested against the old and new engines; never silently reinterpret a SeaWorld
file as a WiredTiger home. A rollback plan must account for writes made after
migration, not just preserve the original file.
