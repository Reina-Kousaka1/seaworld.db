# Document model

SeaWorld's internal record is a document with a stable `DocumentId` and a map of UTF-8 field names to values. The ID is separate from user fields and is stable across close/reopen. IDs in the first implementation are positive 64-bit values unique within one database.

The value algebra is:

- null
- boolean
- signed 64-bit integer
- finite 64-bit floating point
- UTF-8 string
- binary bytes
- ordered array of values
- object/document mapping field names to values

Arrays and nested objects are first-class values. The first format stores object fields in sorted key order for deterministic encoding. Values and documents presented through the API are immutable snapshots; update writes a replacement document.

## Bounds

The initial decoder uses explicit limits: 16 MiB per document, 17 MiB per record block, 128 MiB per database file, nesting depth 64, at most 100,000 values per container and active documents per database, 10,000 collections per database, 1,024 UTF-8 bytes per field name, and 63 ASCII characters per collection name. Limits are checked before allocating from disk-supplied lengths. Invalid UTF-8, duplicate object keys, invalid numeric encodings, and trailing bytes are reported as data errors. The engine can write a compacted copy containing active documents, but it does not replace the open source file or switch paths automatically. The source append-log limit therefore remains in effect until an application explicitly opens the copy.

These limits are protective initial defaults, not a long-term resource-governance system. They will need review against larger workloads before the file format stabilizes.

## Flexible schema

Collections do not impose a global schema in the first version. A document may omit a field or store different value types in the same field across documents. Schema validation and constraints are planned as explicit metadata layered on the document model, not a reason to make SQL the storage representation.

For queries, a missing field is distinct from a present field whose value is null. In SQL V0, `field = NULL` matches explicit null, while `field != value` also matches a missing field; `IS NULL` syntax is not implemented. Nested paths such as `profile.career` traverse object values only. Arrays remain document values and path traversal does not enter array elements. See [SQL V0 semantics](sql.md); SQL and native queries share the same query matcher.

Native `Filter::in_values` builds an OR of exact equality predicates. Values are
not coerced and arrays compare as complete values; this is not an array-element
or multikey query. An empty list matches nothing. `Filter::not_in_values` negates
that predicate, so it includes missing fields and an empty list matches all
documents. An explicit null matches only an explicit null. The normal filter
complexity and value limits are checked on execution, and these expressions use
the existing planner and full-filter evaluation.

Native `Filter::ArrayContains` checks equality against direct elements of the
array at a path. It does not flatten nested arrays or traverse arrays in paths;
missing, null and scalar fields do not match. Nested array or object elements
can be compared as complete values. Negation uses the ordinary `Not` semantics.
It does not introduce multikey indexes. The planner can use a separate indexed
conjunct and always rechecks the complete predicate.

Native `Projection` returns stable IDs and a map of requested `FieldPath` values.
Missing paths are omitted, while null is retained; arrays are returned as whole
values and paths traverse objects only. Projection happens after filtering,
sorting and pagination and is available on both database handles and snapshots.

`Transaction::update_fields` applies up to 1,024 ordered set, unset, increment
or decrement operations to a copy before staging a replacement. A failed update
leaves the staged document unchanged; transaction commit and rollback retain
their existing behavior. Set and numeric operations create missing parent objects;
unset on a missing path is a no-op. Traversing a scalar or array is an error.
Numeric updates require matching integer or finite float types, use zero for a
missing leaf, reject integer overflow and non-finite float results, and do not
coerce types. Index constraints are checked by the existing transaction layer.

`Database::update_one`, `update_many`, `delete_one` and `delete_many` select
the query's committed view, respecting sorting, offset and limit, then stage all
selected changes in one transaction. The `one` variants additionally cap the
limit at one. If an operation or commit fails, no partial successful result is
reported. These convenience methods require a writable V2–V4 handle; they do
not query a caller's already-staged transaction. `upsert_one` updates the first
selected document or inserts an explicitly supplied base document after applying
the same operations. It never infers inserted fields from the filter and does not
require the supplied document to match that filter. It rejects a zero query
limit and commits either path in one transaction; its boolean result indicates
insertion. Retrying an uncertain commit is not idempotent.
