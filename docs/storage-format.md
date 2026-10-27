# Storage formats: versions 1, 2, and 3

This document describes the first SeaWorld file format. It is a custom format, not a serialization dump of Java objects. Integer widths, byte order, lengths, tags, and checksums are encoded explicitly. A version change or incompatible feature requires a version bump or a documented feature flag.

The filename convention is `*.seaworld.db`. The current implementation uses a single file whose source of truth is an append-only sequence of operation blocks. It does not embed SQLite, PostgreSQL, MongoDB, H2, or Derby.

## Header

The fixed header is 64 bytes:

| Offset | Size | Field | Version 1 value/meaning |
| ---: | ---: | --- | --- |
| 0 | 8 | Magic | ASCII `SWDB`, CR, LF, `0x1A`, LF |
| 8 | 2 | Format version | Unsigned integer, big-endian; currently `1` |
| 10 | 2 | Header length | Unsigned integer; `64` |
| 12 | 4 | Block size | Unsigned integer; `4096` nominal bytes |
| 16 | 8 | Feature flags | Unsigned bit set; currently zero, unknown required bits reject open |
| 24 | 16 | Database identifier | UUID bytes; informational identifier, not a secret |
| 40 | 20 | Reserved | Must be zero in version 1 |
| 60 | 4 | Header checksum | CRC32C over bytes 0 through 59 |

The header is written and synchronized before a newly created database handle is returned. Wrong magic, version, header length, reserved bytes, flags, or checksum produces a format/integrity error; the engine does not guess a version.

## Record blocks

Records are appended in order. Each block has a 20-byte prefix, a payload, and a 4-byte CRC32C trailer:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `SWBL` |
| 4 | 1 | Record kind: collection-create, document-insert, document-update, document-delete |
| 5 | 1 | Flags; zero in version 1 |
| 6 | 2 | Reserved; zero in version 1 |
| 8 | 4 | Total block length including prefix and trailer |
| 12 | 8 | Monotonically increasing sequence number, starting at 1 |
| 20 | N | Record-specific payload |
| 20+N | 4 | CRC32C over the prefix and payload |

All integers in the file are big-endian. The total block length is limited to 17 MiB, allowing a document payload of up to 16 MiB plus its record metadata. Record payloads use explicit field encodings. Documents use tagged values and length-prefixed UTF-8/binary values. Object keys are encoded in sorted order for deterministic output. The decoder rejects trailing payload bytes, invalid tags, invalid UTF-8, out-of-range lengths, duplicate object keys, and nesting over the configured limit.

The `block size` field establishes a storage sizing unit for this early format. Version 1 has no page allocator, page directory, B-tree, or fixed-page layout; records may span the nominal block boundary. A later page-based format will need a versioned migration or a new file format rather than silently reinterpreting version 1.

## Version 2 transaction records

V2 keeps the same 64-byte header layout but stores version `2` at offset 8. V1 remains readable and writable with its original record grammar. `Database::create_v2` explicitly creates V2. V2 records never appear under a V1 header.

V2 reuses record kinds 1–4 for collection and document mutations, but each mutation payload starts with its transaction ID. Kind 5 is `TX_BEGIN(transaction_id)`, kind 6 is `TX_COMMIT(transaction_id)`, kind 7 persists an index definition, and kind 8 stores the next document-ID watermark in a compacted snapshot. An index-definition payload contains the positive index ID, positive collection ID, a one-byte unique flag, a positive segment count, then length-prefixed UTF-8 path segments. The watermark contains the next positive document ID and a one-byte exhausted flag. Every record retains the V1 prefix, increasing sequence, and CRC32C framing. A V2 transaction is one contiguous batch with one begin marker, bounded mutation records, and one final commit marker. Index definitions and watermarks are standalone records outside transactions.

On recovery, only a complete, semantically valid transaction with a matching commit marker is applied. An uncommitted suffix is truncated from its begin marker; a partial record inside that suffix discards the entire transaction. A complete checksum failure or invalid transaction sequence is a corruption error, not a recoverable tail. V2 bounds one transaction to 10,000 operations and 64 MiB of encoded operation payloads. Index definitions are durable metadata; entries are rebuilt from committed documents.

`Database::create_v2` writes V2. `Database::create_v1` is an explicit compatibility path. Opening V1 preserves its format and single-operation CRUD semantics; transaction and index APIs require an explicit V2-or-later compacted copy. The original V1 fixture remains the compatibility oracle.

## Version 3 commit watermark

V3 keeps the V2 header and transaction record grammar and stores version `3` at offset 8. New `Database::create` files use V3. V3 accepts the same transaction, index-definition, and document-ID watermark records as V2, plus record kind 9, `VERSION_WATERMARK(committed_version)`. A watermark is a positive signed 64-bit version, appears outside a transaction, cannot move the commit version backwards, and reserves the next transaction ID after the watermark. It is written by V3 compaction so rebuilding a log cannot reuse old snapshot versions. V1 and V2 readers reject a V3 header before replay.

V3 transaction IDs are also the commit-version numbers during normal operation. A successful transaction advances the visible version only after its commit marker is synchronized and applied. Rollback and discarded incomplete transactions do not advance it. V3 compaction can advance the watermark when its snapshot rewrite requires more transactions than the source, but never moves it backwards. `CommitVersion` is separate from `DocumentId` in the public API.

Index-definition records remain standalone records outside transactions. Their ordered entries are rebuilt from committed documents on reopen. V3 does not yet persist index-entry deltas or B+Tree pages.

## Checksums and recovery

CRC32C detects accidental damage to the header and each complete record. It is not a cryptographic integrity mechanism. A complete record with a checksum mismatch, an invalid sequence, or invalid operation semantics fails open and is never silently skipped.

If the final block prefix or declared block is cut off by end-of-file, a writable open truncates only that incomplete tail to the last verified record boundary and calls Rust `File::sync_all()`. A read-only open returns `RecoveryRequired` instead of modifying the file. Shared/exclusive OS locks on the database file prevent a cooperating reader from replaying during an append or recovery. The returned recovery report exposes the number of discarded bytes. A truncated header, a malformed complete prefix, or a complete block with bad checksum is an error. Data in the unfinished final operation may be lost; earlier verified records remain readable. Repeating recovery on the resulting file should discard zero bytes.

V1 mutations append one operation block and call `File::sync_all()` before updating the in-memory view. V2 commits append a contiguous begin/mutation/commit batch and synchronize it before updating memory. If a process dies after a complete V2 commit marker reaches the file but before the caller receives success, the transaction may be present after reopen. Rust's `sync_all` asks the OS to synchronize file content and metadata, corresponding to the Java reference's `FileChannel.force(true)` at the file level. On Unix, creation also synchronizes the parent directory; the portable Rust standard library has no directory-sync API on Windows. Filesystem, device, and OS guarantees still need platform-specific testing; process-kill tests do not simulate power loss.

The Rust test-only `crash-test` feature enables a separate `crash_writer` process with termination points before and during V1-compatible appends, at V2 transaction boundaries, and during suffix recovery. The feature is disabled in release builds unless explicitly requested.

## Compacted copies

`write_compacted_copy` writes a new file in the source version. For V2/V3 sources it retains committed live documents, index definitions, and the document-ID high-water mark; for V1 sources it preserves the legacy V1 grammar. `write_v2_compacted_copy` writes V2 from any source, and `write_v3_compacted_copy` writes V3 and its commit watermark. V3 compaction is rejected while snapshots are active. Compaction preserves the database UUID and live collection/document IDs, and starts a fresh record sequence. The open source remains unchanged. The engine writes and synchronizes a same-directory temporary file, then publishes it through a no-overwrite hard link and synchronizes the directory where supported. The destination filesystem must support hard links. There is no in-place replacement or automatic path switch; the caller explicitly opens the new file and decides when to retire the old one.

## Format evolution

- Reject unsupported major versions and unknown required flags.
- Do not write Java object layouts, Java serialization streams, class names, or platform-sized integers.
- New required fields need a format-version change or a well-defined feature bit and default.
- Before stabilizing the format, add golden byte fixtures, version migration tooling, and crash-injection coverage.
