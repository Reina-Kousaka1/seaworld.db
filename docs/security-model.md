# Security model and threat boundaries

SeaWorld DB is an experimental embedded engine. Its current validation reduces common parser and corruption risks, but it is not a hardened boundary for untrusted multi-user workloads. CRC32C detects accidental damage; it does not authenticate data or protect against intentional edits.

## Assets and trust boundaries

- The database file contains application documents and metadata; treat it as sensitive user data.
- The caller supplies the file path. Collection names, field names, and document values do not become file paths.
- The engine parses database bytes as untrusted input. Application code remains responsible for deciding which files it opens.
- There is no network listener, telemetry, encryption, authentication, authorization, or automatic outbound communication.

## Threats and current controls

| Threat | Current handling | Remaining limitation |
| --- | --- | --- |
| Invalid or truncated header/record | Exact header, record-size, sequence, and checksum checks; only incomplete final tails are recoverable on writable open. Read-only open returns `RecoveryRequired`. | CRC32C is not cryptographic; complete corruption fails open rather than being repaired. |
| Malformed B+Tree page or root | V4 validates page size/version/type/identity, reserved bytes, checksums, key counts, lengths, ordering, child IDs, generation, tree height, cycles, and reachable document references. `Database::check_integrity()` and `seaworld check` repeat live index/document and retained-page checks without repairing the file. | No authenticated pages. Old unreachable COW pages remain until compaction and are not individually enumerated by the report. |
| Integer overflow / forged length | Checked arithmetic and bounded cursor reads are used before slicing or allocating. | Ongoing review is needed for every future format extension. |
| Oversized document or nesting bomb | Documents are bounded to 16 MiB, nesting to 64, and collection/container counts and names are limited. Index string keys are bounded to 1,024 UTF-8 bytes. | Query and page-count budgets are conservative fixed limits, not a complete process-wide resource quota system. |
| Invalid UTF-8 or duplicate object field | UTF-8 is validated; duplicate decoded fields are rejected. | No normalization of Unicode identifiers is performed. |
| Malformed SQL | The SQL V0 lexer/parser uses input, token, string, nesting, and statement-count bounds and reports byte offsets through typed query errors. It translates to the native query/transaction path. | No SQL comments, multiple statements, joins, grouping, functions, aliases, index DDL, or full standards compatibility. Dedicated parser fuzzing remains future work. |
| Path traversal | Engine-generated filenames are not derived from document data or collection names. | The caller controls the path and must apply its own path policy. |
| Symlink / TOCTOU attacks | Writer locks and no-overwrite creation reduce cooperating-process races. | Path canonicalization is not a defense against hostile parent directories, symlink replacement, or all TOCTOU races. Place files in directories controlled by the application. |
| Corrupt or malicious backup/migration input | `Database::backup()` checks the live state and writes a consistent compacted V4 copy to a new path. `Database::restore_backup()` validates through a read-only open and also refuses to overwrite. | No automatic backup retention. Do not treat backup files as trusted. |
| Resource exhaustion through repeated writes | File, document, page, operation-count, transaction-size, snapshot, nesting, and key limits bound core structures. | Repeated append-only COW writes can consume the database's 128 MiB file budget before compaction. No CPU quota or admission-control layer exists. |
| Malformed database/recovery input | Deterministic property tests open bounded arbitrary files and tails, preserve a prior committed document when recovery can open, and reject checksummed-but-invalid transaction sequences without panicking or modifying the input. Existing document codec properties cover valid and malformed documents. | This is bounded CI fuzzing, not exhaustive input coverage or a dedicated libFuzzer corpus. |
| Process interruption | The recovery log commits document changes and V4 root publications atomically at a commit marker. Process-kill cases cover bulk update, bulk delete, upsert, and mixed insert/update/delete transactions at eight append/commit/publish boundaries, and verify only the whole old or whole new state is visible. | Process-kill tests do not establish OS-crash, power-loss, filesystem, or device-cache guarantees. |

## Safe Rust and panic policy

The current engine uses safe Rust and has no `unsafe` blocks in the core. Database-file and query input errors should return typed `Error` values. Panics in tests indicate a failed invariant; untrusted bytes must not be decoded with unchecked indexing or unbounded allocation.

## Future network boundary

A future server must reuse the same engine and separately design authentication, authorization, TLS, rate limits, connection limits, and request-size limits. SeaWorld will use established cryptographic libraries rather than custom cryptography. None of these controls exists today.

## Validation work still needed

- Dedicated libFuzzer targets and a corpus for header, record, document, page,
  and recovery decoders.
- More precise reporting for unreachable/corrupt pages and wider read-only CLI corruption tests.
- More adversarial tests for page references, nested input, path races, backup files, and migration inputs.
- Platform-specific durability tests beyond process termination.
