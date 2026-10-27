# Storage format: version 1

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

## Checksums and recovery

CRC32C detects accidental damage to the header and each complete record. It is not a cryptographic integrity mechanism. A complete record with a checksum mismatch, an invalid sequence, or invalid operation semantics fails open and is never silently skipped.

If the final block prefix or declared block is cut off by end-of-file, open truncates only that incomplete tail to the last verified record boundary and calls Rust `File::sync_all()`. The returned recovery report exposes the number of discarded bytes. A truncated header, a malformed complete prefix, or a complete block with bad checksum is an error. Data in the unfinished final operation may be lost; earlier verified records remain readable. Repeating recovery on the resulting file should discard zero bytes.

Each successful mutation appends one complete operation block and calls `File::sync_all()` before updating the in-memory view. This is not a multi-operation transaction. If a process dies after a complete record reaches durable storage but before the caller receives success, the operation may be present after reopen. Rust's `sync_all` asks the OS to synchronize file content and metadata, corresponding to the Java reference's `FileChannel.force(true)` at the file level. On Unix, creation also synchronizes the parent directory; the portable Rust standard library has no directory-sync API on Windows. Filesystem, device, and OS guarantees still need platform-specific testing; process-kill tests do not simulate power loss.

The Rust test-only `crash-test` feature enables a separate `crash_writer` process with termination points before a block write, partway through a block, after a complete write but before sync, and after sync but before returning. The feature is disabled in release builds unless explicitly requested.

## Format evolution

- Reject unsupported major versions and unknown required flags.
- Do not write Java object layouts, Java serialization streams, class names, or platform-sized integers.
- New required fields need a format-version change or a well-defined feature bit and default.
- Before stabilizing the format, add golden byte fixtures, version migration tooling, and crash-injection coverage.
