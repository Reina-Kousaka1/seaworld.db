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

The initial decoder uses explicit limits: 16 MiB per document, 17 MiB per record block, 128 MiB per database file, nesting depth 64, at most 100,000 values per container and active documents per database, 10,000 collections per database, 1,024 UTF-8 bytes per field name, and 63 ASCII characters per collection name. Limits are checked before allocating from disk-supplied lengths. Invalid UTF-8, duplicate object keys, invalid numeric encodings, and trailing bytes are reported as data errors. There is no compaction yet, so the append-only file limit applies even when documents have been deleted or replaced many times.

These limits are protective initial defaults, not a long-term resource-governance system. They will need review against larger workloads before the file format stabilizes.

## Flexible schema

Collections do not impose a global schema in the first version. A document may omit a field or store different value types in the same field across documents. Schema validation and constraints are planned as explicit metadata layered on the document model, not a reason to make SQL the storage representation.

For queries, a missing field is distinct from a present field whose value is null. The SQL adapter will document that `field IS NULL` matches explicit null and that a separate predicate is needed for missing fields. Nested paths such as `profile.career` traverse object values only. Array traversal semantics are not yet defined; SQL and native queries must share the same eventual rule.
