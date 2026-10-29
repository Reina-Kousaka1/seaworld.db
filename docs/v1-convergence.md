# V1 convergence and public boundary

This is an assessment, not a V1 release or a new wire protocol. The current
Rust `Database`, `Transaction`, `Snapshot`, typed `Query`, `Document` and
`Error` APIs are experimental. Native and SQL queries share the existing engine.
Backend selection, a stable cross-language ABI and network transport do not yet
exist. See [WiredTiger assessment](wiredtiger-assessment.md) before changing
storage responsibilities.

The crate now exposes an API foundation version, capability descriptor,
bounded request correlation ID and typed error categories. These are
transport-neutral building blocks, not a server or a completed stable V1
protocol. Capabilities describe engine features; transactions and field updates
require a transactional V2–V4 database. Request IDs do not imply deduplication.

## Candidate transport-neutral contract

An eventual versioned boundary should expose version and capabilities; health
with no implicit repair; collections; document reads, queries, inserts, updates
and deletes; and explicit transaction lifetimes. Keep backup, restore,
compaction and other administration separate from ordinary data operations.
Consumers should never need direct storage-file access. Existing typed Rust
errors are useful locally but their text and enum are not yet a stable protocol
code registry. Request/operation IDs and external write idempotency are not
implemented: a correct design needs durable, bounded deduplication tied
atomically to the write, a defined retention window and behavior for retries
after an uncertain commit. A request ID alone is not idempotency. Do not promise
exactly-once delivery or automatic retries. REST, sockets and FFI remain open.

## V1 blockers to resolve

- Define and test a versioned public API contract, stable error categories,
  lifecycle, compatibility policy and operational limits.
- Expand deterministic and generated isolation, corruption, concurrency,
  migration and process-crash coverage; add fuzzing and a supported-platform
  qualification strategy. Process-kill tests alone do not prove power-loss
  durability. Establish the actual filesystem and hardware assumptions.
- Decide a supported document query/update contract, including missing versus
  null, arrays, projections, field updates and atomic bulk behavior; implement
  and differentially test the chosen semantics with indexes and snapshots.
- Establish a documented recovery, backup, restore and upgrade policy with
  verified V1–V4 and Java V1 compatibility and explicit migration.
- Review resource ceilings and security before any production-readiness claim.

## Post-V1 candidates

Multikey, text and compound indexes, expanded SQL, in-memory storage, additional
drivers and network service can be considered separately; they are not implied
by the current crate. Their priority depends on the eventual V1 contract.

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
