# Public API foundation contract

This document describes the current transport-neutral Rust foundation. It is
not a network protocol, a foreign-function ABI, or a promise that SeaWorld has
released V1. It deliberately hides the append log, B+Tree pages, index entries,
borrowed executor rows, cursors, and file handles so a future backend can meet
the same public contract.

## Lifecycle and metadata

`PublicEngine::api_info` returns the public foundation version and an explicit
capability descriptor. `health` reports an already-open engine as `Ready`; open,
recovery, and validation errors are returned before an engine exists. A driver
must not infer health from a filesystem path or from a successful previous
request. The public data surface provides collection listing/creation, owned
document reads, query, projected query, insert, update, delete, upsert, and an
explicit transaction lifetime. Backup, restore, compaction, migration, and
integrity inspection are administration APIs, not ordinary client operations.

`OperationContext` and `RequestId` carry caller correlation only. SeaWorld does
not persist IDs, deduplicate writes, provide exactly-once semantics, or make an
uncertain commit safe to retry. Durable external idempotency needs a separate,
bounded, atomic design and remains an architecture decision.

## Query results

`PublicQueryRow` is owned and backend-neutral. Every row has a stable
`DocumentId` and either a complete `Document` or a projected field map. A
projected map contains requested paths that are present after query filtering,
sorting, offset, and limit. Missing paths are omitted; explicit `Value::Null`
is retained; an empty map is therefore a valid result. Arrays are returned as
whole values. This distinction is independent of whether the planner selected a
scan, an ID lookup, or a secondary-index candidate.

## Transactions

`PublicTransaction` is tied to a mutable engine handle. Its document reads see
its own staged writes. A successful `commit` atomically publishes staged
operations according to the V2-V4 transaction protocol; `rollback` and drop
discard them. Both methods consume the transaction, so a completed transaction
cannot be reused, committed twice, or rolled back afterwards through this
contract. A failed mutation leaves the transaction's previous staged state in
place. Constraint and resource-limit failures during a transaction must be
handled by the caller; SeaWorld does not claim serializable isolation or
predicate locking.

The current public transaction interface intentionally exposes direct,
ID-addressed mutations rather than bulk query mutations. `PublicEngine`
provides `update_one`, `update_many`, `delete_one`, `delete_many`, and
`upsert_one`; they retain the database implementation's atomic bulk semantics.
`upsert_one` either updates the first selected document or inserts the explicit
base document after field operations. It never derives inserted fields from the
filter.

## Errors and capability evolution

`Error::code()` produces a stable machine-readable category; callers must not
parse human-readable text. The code set distinguishes invalid arguments,
resource limits, not-found, lock/conflict states, unique constraints,
transaction conflicts and states, unsupported operations and capabilities,
unknown format versions, incompatible required features, corrupt format,
integrity failures, recovery requirements, I/O, and internal invariants.
Unknown/newer file versions are `unknown_format_version`, not corruption.

`ErrorCode` is marked non-exhaustive. New codes can be additive in a later
foundation version; consumers must include a fallback path. Capability names
are lowercase ASCII with underscores. `Capabilities::supports` returns `Some`
for every known name (including known unsupported features) and `None` for an
unknown future name. Older clients must preserve that distinction and must not
interpret `None` as false.

Additive methods, result fields, error codes, and capabilities require a public
foundation-version update and tests. Renaming/removing a capability, changing a
stable error-code meaning, silently changing result semantics, or changing a
file format without a new version/required feature is a breaking change.
Deprecation must retain the old behavior through the stated compatibility
period; no such deprecation is active today.

## Backend and format boundary

The current `Database` is the sole implementation of `PublicEngine`. A later
backend may implement the same owned values and error/capability contract, but
cannot expose backend handles or silently reinterpret existing `*.seaworld.db`
files. WiredTiger is not enabled, integrated, or required by this contract.
