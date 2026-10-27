# Secondary indexes

## Definitions and persistence

V2 supports single-field indexes and unique single-field indexes. An index definition stores the collection ID, normalized `FieldPath`, index ID, and unique flag. Definitions are written as checksummed V2 metadata records; index entries are rebuilt from committed documents on open and after recovery. The file does not store B-tree or hash-table memory layouts.

This rebuild-on-open choice keeps the durable format small and lets the engine validate the index against its source documents. It trades startup time for simpler corruption handling. A duplicate or unsupported value encountered while rebuilding a persisted unique index is an integrity error.

## Paths and values

Paths traverse nested objects only. Missing values are omitted from the index and may occur in many documents. Explicit `Null` is a value and is indexed; a unique index permits only one explicit null. Boolean, Int64, finite Float64, String, and Null are indexable. Binary, Array, and Object are not: index creation fails if a present field has one of those values, and later writes that would place one in an indexed path fail with an unsupported-index-value error.

Float keys use total numeric ordering for finite values and treat negative and positive zero as equal. Strings use Rust's UTF-8 byte lexicographic order; no locale collation is applied. Comparisons never coerce between value types.

## Constraints and consistency

Creating an index scans the collection before appending its definition. A unique-index creation fails without changing the file if it finds duplicate indexed values. Unique constraints are checked immediately for each staged transaction operation against the transaction's current view. Every committed mutation updates the document state, primary-ID map, and all secondary indexes together after the commit marker has synchronized. Recovery and compaction rebuild the same entries from committed logical state.

Indexes are available only in V2. A V1 file must first be copied to V2 through the documented non-overwriting upgrade/compaction API.

## Limits and current scope

The engine caps definitions per database and bounds path depth and segment bytes. Compound, text, geospatial, array-element, and persistent page indexes are not implemented. The current planner uses secondary indexes for equality predicates; other operators remain correct through scans.
