# Persistent index storage decision

## Current implementation: durable definitions, rebuilt entries

V2 and V3 persist index definitions in the SeaWorld append log. Index entries live in an ordered in-memory BTreeMap and are rebuilt from committed documents on reopen. Document transactions update the in-memory entries after the transaction's commit marker is synchronized. Unique indexes are checked against the staged transaction view. This keeps the current-state query engine correct after reopen, but index contents themselves are not persisted as records or pages and rebuild cost grows with the collection.

Index-entry deltas in the same transaction log remain a candidate for the next engine wave. If implemented, replay must validate their exact relationship to document mutations before publishing a commit. Today, document data is the source of truth and replay rebuilds each index from it.

The ordered in-memory index supports equality and range candidate lookup. Index candidates are still checked against the complete filter. Sorting currently sorts query results; it does not stream directly from an index.

## Persistent B+Tree page design candidate

The append log makes index changes durable but does not yet provide a page allocator or on-disk B+Tree. The next storage-format revision should use a copy-on-write B+Tree whose root is published by a committed log record. A proposed page is 4,096 bytes with a fixed header: `SWIP` magic, page-format version, page type, flags, page ID, key count, payload length, and CRC32C. Remaining bytes are zero-filled reserved space except for the CRC trailer.

- **Leaf pages:** sorted `(IndexValue, DocumentId)` entries, collection/index identity, and optional validated left/right sibling IDs for range scans.
- **Internal pages:** a first child ID followed by sorted separator-entry/child-ID pairs. Separators use the same typed ordering as the query/index layer.
- **Root metadata:** an index catalog maps each `IndexId` to its collection, normalized field path, unique flag, root page ID, and generation. A transaction writes new pages first and commits root changes in the append log; uncommitted pages are unreachable and reclaimable by a later compaction.
- **Validation:** bound page count, key count, B+Tree height (32), payload lengths, child IDs, sibling IDs, key ordering, database UUID, and generation before allocation or traversal. Reject cycles and invalid references. Page CRC32C detects accidental damage; it is not cryptographic authentication.

The page tree is a design target, not an implemented feature. Adding it requires an explicit new file-format version and golden fixtures; V1, V2, and V3 must remain readable. No in-memory Rust container layout is written directly to disk.

## Query semantics

The ordered comparison domain is `Null < Bool < Int64 < finite Float64 < UTF-8 String`, with comparison only within compatible value types. Missing values have no index entry; explicit null does. Float keys treat negative and positive zero as equal. Compound, text, geospatial, and array-element indexes remain out of scope.
