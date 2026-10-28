# Persistent index storage

## V4 implementation

Format V4 stores immutable 4 KiB B+Tree pages in the same checksummed append-only SeaWorld file as collections and documents. V1, V2, and V3 remain readable. `Database::create` creates V4; explicit V1/V2/V3 constructors are compatibility tools. V2/V3 indexes continue to rebuild their in-memory entries from documents.

The tree key is `(typed IndexValue, DocumentId)`. The order is `Null < Bool < Int64 < finite Float64 < UTF-8 String`, comparing only like types. Missing fields have no index entry; explicit `null` has one. Float keys treat `-0.0` and `+0.0` as equal. A string index key is limited to 1,024 UTF-8 bytes. Each index is a single-field index; compound, text, and array-element indexes are not implemented.

A page is explicitly encoded, never a raw Rust layout:

| Offset | Bytes | Meaning |
| ---: | ---: | --- |
| 0 | 4 | `SWIP` magic |
| 4 | 1 | Page version (`1`) |
| 5 | 1 | Page type (`1` leaf, `2` internal) |
| 6 | 2 | Required-zero flags |
| 8 | 8 | Positive page ID, big-endian |
| 16 | 8 | Positive index ID |
| 24 | 8 | Non-negative transaction generation |
| 32 | 2 | Key/separator count |
| 34 | 2 | Payload byte count |
| 36 | 4 | Reserved zero bytes |
| 40 | 4,052 max | Explicit key/child encoding followed by zero padding |
| 4,092 | 4 | CRC32C over bytes 0 through 4,091 |

Leaf payloads contain ordered typed keys and positive document IDs. Internal payloads contain the first child ID, followed by ordered separator keys and right-child IDs. A child reference must be a positive bounded ID and resolve when traversed from a root. Tree validation checks page checksum, identity, generation, page type, key count, payload bounds, strict ordering, child ranges, cycles/shared children, and a maximum height of 32. CRC32C detects accidental damage, not malicious modification.

Page records are log records containing exactly one page. Index creation appends and synchronizes pages first, then publishes an index definition with the root page and generation. If publication is absent after a crash, those pages are unreachable and harmless. Document transactions append COW pages and root-publication records inside the same `TX_BEGIN` / `TX_COMMIT` batch as the document changes. Recovery applies neither pages nor roots from an uncommitted batch. A complete commit exposes the new documents and roots together; a torn or uncommitted suffix is discarded from the begin record.

A COW update clones only the affected search paths and splits full leaves/internal nodes. Old pages remain immutable. The log can therefore contain unreachable old and intermediate pages; they are retained until V4 compaction writes a compact copy. This design prioritizes one-file root publication over immediate page reclamation. Page IDs are monotonic within a file and bounded to 32,768 pages; database and transaction byte limits may be reached earlier.

On V4 reopen, SeaWorld loads the page records and traverses the persisted roots to repopulate the in-memory ordered index map. It does not rebuild index entries by scanning every document. Each loaded entry is checked against its referenced live document. Query execution still rechecks all candidates against the full filter. V4 compaction builds fresh roots from the current live state and preserves the commit watermark; active snapshots cause compaction to be rejected. The embedded API exposes `Database::indexes()` and `Database::check_integrity()`; the `seaworld` shell/CLI also provides read-only checking and index metadata inspection.

## Recovery and boundaries

The commit marker is the logical publication point; `File::sync_all()` is the success boundary reported by the API. Process-kill tests cover termination before/during page writes, after pages but before root publication, during root publication, after roots but before commit, and existing commit/sync boundaries. These tests do not prove power-loss guarantees or device flush behavior. V4 is experimental and does not claim full ACID or production durability.

`Database::check_integrity()` rechecks live document/primary-index consistency, secondary-index contents, reachable B+Tree ordering, and the checksums and identities of retained pages without repairing the source. It intentionally permits unreachable old copy-on-write pages because those remain in the append-only log until compaction. It reports their total retained page count rather than treating valid historical pages as corruption. `Database::backup()`, `Database::verify_backup()`, and `Database::restore_backup()` are described in [backup and restore](backup.md); publication requires a new destination.
