# V1 convergence and public boundary

This records the V1 release freeze, not a network protocol or a new storage
architecture. Package version 1.0.0 and public API version 1.0.0 are frozen;
the documented Rust `Database`, `Transaction`, `Snapshot`, typed `Query`,
`Document`, and `ErrorCode` semantics are stable within API major version 1.
Native and SQL queries share the existing engine. Backend selection, a stable
cross-language ABI and network transport do not yet exist. See [WiredTiger
assessment](wiredtiger-assessment.md) before changing storage responsibilities.

The crate exposes an API version, capability descriptor,
bounded request correlation ID, typed error categories, and owned
backend-neutral public query rows. These are
transport-neutral building blocks, not a server or wire protocol. Capabilities
describe engine features; transactions and field updates
require a transactional V2–V4 database. Request IDs do not imply deduplication.
`PublicEngine` and `PublicTransaction` provide an engine-independent Rust
interface for collection access, owned reads, full and projected queries,
one/many mutations, upserts, and transactions. The current `Database`
implements this boundary; no second backend is provided.
The interface does not provide a stable foreign-language ABI or wire protocol.

## Frozen transport-neutral contract

The versioned boundary exposes version and capabilities; health
with no implicit repair; collections; document reads, queries, inserts, updates
and deletes; and explicit transaction lifetimes. Keep backup, restore,
compaction and other administration separate from ordinary data operations.
Consumers should never need direct storage-file access. Existing typed Rust
errors are useful locally, while `ErrorCode` is the stable machine-readable
registry. Request/operation IDs and external write idempotency are not
implemented: a correct design needs durable, bounded deduplication tied
atomically to the write, a defined retention window and behavior for retries
after an uncertain commit. A request ID alone is not idempotency. Do not promise
exactly-once delivery or automatic retries. REST, sockets and FFI remain open.

## V1 release-freeze result

The technical qualification gate currently covers the documented document,
query, update, transaction, recovery, backup/restore, migration, limit and
platform contracts. It includes bounded generated malformed database/recovery
inputs and process-kill cases for composite document operations. It does not
claim exhaustive fuzzing or power-loss qualification.

The public Rust contract and package/API version are frozen at 1.0.0. The
Linux/Windows release gate must remain green for the exact release commit. This
does not require a transport, foreign-language ABI, or a WiredTiger backend.

## Post-V1 candidates

Multikey, text and compound indexes, expanded SQL, in-memory storage, additional
drivers and network service can be considered separately; they are not implied
by the current crate. Dedicated libFuzzer and broader storage/power-loss
qualification are defense-in-depth work after the bounded release gate.

## Architecture decisions

The owner must decide whether and how a WiredTiger backend is activated, who
owns transactions, snapshots, indexes and recovery, and how migration and Rust
FFI safety are handled. Serializable isolation, predicate locking, multikey
index semantics, stable external idempotency retention, and transport and
authentication need separate decisions before implementation. No such choice
is made by this assessment.

## Experiments

A disposable, independently built WiredTiger adapter prototype and ARM64
qualification can test the proposed boundary once approved. It must not change
the existing backend, data files, public semantics or production systems.
