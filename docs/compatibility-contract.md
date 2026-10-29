# Compatibility contract and release gate

This describes the present experimental contract and conditions for a future
stable V1 release. It does not declare V1 ready or promise compatibility for
unstated formats, APIs, hardware, or filesystems.

## Format compatibility

- New databases use SeaWorld format V4; V1, V2, V3 and V4 are readable. The
  checked-in Java-generated V1 fixture remains a compatibility oracle. V1
  retains single-operation CRUD; transactions and indexes require a copy to
  V2 or newer, and owning snapshots require V3 or newer.
- Opening an older file does not silently migrate it. An unknown format version
  is rejected as `Error::UnsupportedFormatVersion`, mapped to the stable
  `unknown_format_version` category. Unsupported required flags are
  `Error::UnsupportedFeature`, mapped to `incompatible_format`. Malformed
  headers remain `corrupt_format`, and checksum or semantic failures are
  `integrity_failure`; clients must use the code rather than parse diagnostics.
- Compacted-copy migration is explicit and does not overwrite an existing target.
  Verify the new copy and retain the source until the application accepts it.
  Changes that make existing bytes unreadable or reinterpret their meaning need
  a new format version or an explicitly negotiated required feature flag.
  Changes confined to in-memory algorithms, an additive API or query evaluation
  need not change the file version, but still require semantic compatibility tests.
- Backup, restore and compaction preserve the database UUID, live collection and
  document IDs, index definitions and committed document-ID high-water mark.
  V3/V4 commit-version watermarks never move backwards and may advance during a
  copy. Copies can diverge under the same UUID; later allocations in one copy
  are not known to the others. Rolled-back provisional IDs may be reused.

## Limits and durability

The encoded document limit is 16 MiB; an encoded record is at most 17 MiB,
and a database file at most 128 MiB. One transaction is limited to 10,000
operations and 64 MiB of encoded operation data, including V4 index publication
records. Bulk operations use one transaction and can therefore fail at these
limits without publishing a partial result. Query result limit and offset are
at most 100,000; filter depth is 64 and node count 1,024. Paths have at most 64
segments; projections accept at most 1,024 paths. Projected query results have
a 128 MiB accounting budget for encoded values, path bytes and per-field
overhead. This is a guard, not a precise bound on all process memory; the legacy
direct `Projection::apply` helper does not enforce this budget. Documents and
index state are held in memory, and allocation failures are not recoverable in
every situation. Each intermediate field-update document must fit the document
limit, even if a subsequent operation would shrink it.

Commit synchronizes the log before success; a sync error can leave the outcome
ambiguous and requires reopen. An incomplete tail is recovered on writable open,
while read-only open reports recovery required. Complete corrupt records fail
closed. Copy publication uses same-directory hard links without overwrite and
directory synchronization where supported. Linux and Windows process-kill CI does
not prove survival of OS crashes, power loss, dishonest devices, or arbitrary
filesystems. CRC32C detects accidental damage but does not authenticate data.
See [storage](storage-format.md), [transactions](transactions.md) and
[backups](backup.md), and [platform qualification](platform-support.md) for
details and limitations.

## V1 release gate

### V1-BLOCKER

- Qualify the new public result, transaction, error and capability contract on
  every supported platform and preserve it through a stated compatibility
  window. `PublicQueryRow` is backend-neutral and `ErrorCode` is non-exhaustive;
  see [the public API contract](public-api.md). Request IDs do not imply
  deduplication.
- Qualify generated document mutations and differential indexed queries across
  reopen, snapshots, failure, recovery and compatibility fixtures; expand fuzz
  and process-crash coverage beyond the present tests.
- Keep the Linux/Windows CI qualification and conservative filesystem/durability
  boundary current as release evidence; ARM64 and other platforms remain
  unqualified until separately tested. Preserve the migration compatibility gate.

### POST-V1

- Additional drivers and a transport service, subject to their own security and
  deployment review; they are not required by the current embedded contract.

### ARCHITECTURE DECISION

- Whether and how WiredTiger becomes a backend; multikey-index and serializable
  isolation semantics; durable external idempotency and its retention boundary.

### EXPERIMENT

- A disposable WiredTiger adapter prototype, if separately authorized. No such
  backend, migration or service is part of the present release gate.
