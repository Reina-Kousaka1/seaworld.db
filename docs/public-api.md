# Public API V1 contract

This document freezes the transport-neutral SeaWorld Rust API at **API version
1.0.0**. It is not a network protocol or a foreign-function ABI. It deliberately
hides the append log, B+Tree pages, index entries, borrowed executor rows,
cursors, and file handles so a future backend can meet the same public contract.

Package version, API version, and on-disk version are separate version spaces.
The initial V1 package and API version are both `1.0.0`; new databases use
on-disk format V4 while V1--V3 remain readable. Equal initial package/API
numbers do not require them to change together in the future.

## Frozen public surface

The backend-neutral client boundary consists of `PublicEngine`,
`PublicTransaction`, `ApiInfo`, `Capabilities`, `EngineHealth`, `RequestId`,
`OperationContext`, `PublicQueryRow`, `PublicRowValue`, `ErrorCode`, and
`API_VERSION`. Its input and result values use the documented public document
and query types: `Document`, `Value`, `DocumentId`, `CollectionId`,
`FieldPath`, `Query`, `Filter`, `Projection`, `DocumentOperation`, and their
documented limit constants. `Database`, `Transaction`, `Snapshot`, SQL and
administration types remain supported native embedded-library APIs, but are not
the backend-neutral driver contract.

Public V1 methods preserve their documented signatures and semantics throughout
API major version 1. Additive methods, optional capability names, error codes,
and non-exhaustive result fields may be introduced only in a later compatible
API release with documentation and tests. Removing or renaming a method or
capability, changing a stable error-code meaning, changing documented query or
row semantics, or changing required input/output meaning is breaking and needs
API major version 2. `Document` and `Value` remain the documented fixed V1
value algebra; a new value variant is a breaking change.

## Lifecycle and metadata

`PublicEngine::api_info` returns the public API version and an explicit
capability descriptor. `health` reports an already-open engine as `Ready`; an
owned trait object can be closed explicitly with `close`. Open, recovery, and
validation errors are returned before an engine exists. A driver must not infer
health from a filesystem path or from a successful previous request. The public
data surface provides collection listing/creation, owned
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

## Stable errors

`Error::code()` produces a stable machine-readable category; callers must not
parse human-readable text. `Error` carries Rust-local diagnostics and its
variant payloads, source errors, and display text are not a machine contract.
`ErrorCode` is non-exhaustive: a newer code must follow the caller's fallback
path rather than being parsed or treated as a known code.

| Stable code | Meaning |
| --- | --- |
| `io` | I/O or OS-provided randomness failure. |
| `locked`, `read_only` | Handle or writer access state prevents the operation. |
| `recovery_required` | Writable recovery is required before a read-only open. |
| `corrupt_format`, `integrity_failure` | Malformed bytes or a failed semantic/checksum invariant. |
| `invalid_argument`, `malformed_document`, `resource_limit` | Caller input or a documented bound is invalid. |
| `not_found`, `conflict`, `unique_constraint` | Requested state or a constraint prevents the operation. |
| `transaction_conflict`, `transaction_state` | Snapshot/transaction lifecycle state prevents the operation. |
| `unsupported_operation`, `unsupported_capability` | The requested documented operation or capability is unavailable. |
| `incompatible_format`, `unknown_format_version` | Required feature incompatibility or an unknown newer file version. |
| `backend_unavailable`, `internal_invariant` | Backend availability or an internal invariant failure. |

Unknown/newer file versions are `unknown_format_version`, not corruption.
The table freezes the category names and meanings; a category that no current
embedded path emits is reserved for a conforming future backend rather than a
promise that every operation can return it today.

## Capability contract

V1 guarantees these known capability names and values for the embedded engine:

| Name | V1 value | Meaning |
| --- | --- | --- |
| `native_queries` | supported | Typed native query/filter API. |
| `projections` | supported | Owned document-aware projections. |
| `array_contains` | supported | Direct array-element query semantics. |
| `field_updates` | supported | Set, unset, increment and decrement operations. |
| `transactions` | supported | V2--V4 transaction APIs under the documented one-writer model. |
| `durable_request_deduplication` | unsupported | Request IDs are correlation only, not idempotency keys. |

`Capabilities::supports` returns `Some(true)` or `Some(false)` for these known
names and `None` for an unknown future name. Clients must preserve `None` as
unknown; it is neither support nor a format error. Future capability names are
lowercase ASCII with underscores and are additive only.

## Error and capability evolution

`Error`, `ErrorCode`, `Capabilities`, `ApiInfo`, `EngineHealth`,
`PublicQueryRow`, and `PublicRowValue` are marked non-exhaustive where callers
might otherwise rely on an exhaustive Rust match or literal. New codes can be
additive in a later API version; consumers must include a fallback path.

Additive methods, result fields, error codes, and capabilities require a public
API-version update and tests. Renaming/removing a capability, changing a
stable error-code meaning, silently changing result semantics, or changing a
file format without a new version/required feature is a breaking change.
Deprecation must retain the old behavior throughout API major version 1; no
deprecation is active today.

## Backend and format boundary

The current `Database` is the sole implementation of `PublicEngine`. A later
backend may implement the same owned values and error/capability contract, but
cannot expose backend handles or silently reinterpret existing `*.seaworld.db`
files. WiredTiger is not enabled, integrated, or required by this contract.
