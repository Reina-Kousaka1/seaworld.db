# Roadmap

SeaWorld DB is versioned as `0.0.x` while the file format, recovery, and API are experimental. No `1.0` stability claim is planned until the storage format and recovery behavior are sufficiently tested and documented.

## Phase 0: repository and foundation

- [x] Separate SeaWorld identity and architecture from Tyvera.
- [x] Compare C++, Rust, and Java for an embedded document engine.
- [x] Define the initial versioned header and record-block format.
- [x] Preserve the selected repository's existing AGPL-3.0 license.
- [x] Commit and push the engine foundation to the chosen GitHub repository.
- [x] Create, validate, close, reopen, and read a SeaWorld database file.
- [x] Persist collections and documents with nested objects and arrays.
- [x] Add validation and CI for the actual engine code.
- [x] Migrate the engine core to Rust while preserving the version 1 file format.
- [x] Keep a Java-generated file fixture and compare Rust record bytes against it.
- [x] Add process crash injection around append and sync, then verify repeatable tail recovery.
- [x] Pass Rust CI on Linux and Windows while retaining the Java reference suite.
- [x] Remove the Java engine, Java tests, and transitional Java CI after the Rust compatibility gate passed.
- [x] Add a safe compacted-copy writer that preserves the source and never overwrites its destination.

## Phase 1: transactions, indexes, and native query

- [x] Keep V1 byte-compatible while making new files use a distinct V2 header.
- [x] Group V2 mutations in transactions with begin and commit markers, read-your-writes, rollback, bounded operation count and size, and replay of committed transactions only.
- [x] Recover an interrupted transaction by truncating from its begin record; reject complete checksum or semantic corruption.
- [x] Add process-kill coverage before, during, and after commit, and interrupt recovery to verify repeatability.
- [x] Persist single-field and unique index definitions; rebuild entries from committed documents on open.
- [x] Add a typed native query AST, bounded filters/results, primary/secondary/full-scan planning, explain, sorting, and deterministic results.
- [x] Add non-overwriting compaction in V2 and explicit V1-to-V2 copy migration while preserving UUIDs, live IDs, indexes, and ID high-water state.
- [x] Document the implemented transaction, index, query, format, and security limits without claiming full ACID or SQL support.

## Phase 2: snapshots and indexed ranges

- [x] Keep V1/V2 replay compatible and make V3 the default format with a durable commit-version watermark.
- [x] Add owning MVCC snapshots with bounded leases and retained-history bytes.
- [x] Support one writer and fixed read-only handles with shared/exclusive file locks; reject a second writer.
- [x] Keep snapshot reads stable across committed updates and deletes.
- [x] Add ordered single-field range candidates and recheck every candidate against the full query filter.
- [x] Refuse V3 compaction while snapshot leases are active and preserve/advance the version watermark safely.
- [x] Add deterministic state-model, MVCC, index-range, read-only recovery, and process crash coverage.
- [ ] Add a dedicated libFuzzer harness and broader randomized recovery/input properties.
- [ ] Verify stress behavior under CI on Linux and Windows before making stability claims.

## Phase 3: persistent indexes and embedded experience

- [x] Introduce V4 with checksummed 4 KiB page images and preserve V1/V2/V3 read compatibility.
- [x] Use immutable copy-on-write leaf/internal pages with bounded IDs, key sizes, and tree height.
- [x] Write index pages and root publications in one transaction batch and discard incomplete batches during recovery.
- [x] Add process-kill hooks around page writes, root publication, and transaction commit boundaries.
- [x] Add a V4 compacted-copy path that rebuilds live index trees into a non-overwriting destination.
- [x] Load V4 secondary entries from persisted pages on reopen and compare indexed range results with a full-scan oracle.
- [x] Add deterministic generated B+Tree mutation sequences with an ordered-set oracle using `proptest`.
- [x] Expose read-only index metadata and integrity checks; provide consistent non-overwriting backup and restore-to-new-file library APIs.
- [x] Implement a bounded SQL V0 lexer/parser, document-aware literal values, positional bindings, and SELECT/INSERT/UPDATE/DELETE/CREATE COLLECTION/EXPLAIN SELECT through the native engine.
- [x] Add a SeaWorld shell, metadata inspector, read-only integrity checker, and non-overwriting backup/restore commands over the embedded APIs.
- [ ] Add true in-memory storage without temporary persistent files.
- [ ] Expand security-model, resource-limit, corruption, fuzz, and cross-platform crash coverage.
- [x] Define current connection/session ownership and future driver/protocol boundaries in docs.

## Next engine wave

1. Fuzz malformed headers, record streams, transaction state machines, and nested document decoders.
2. Extend SQL V0 with index DDL and broader prepared-query ergonomics; add migration inspection to the CLI.
3. Add a true memory backend and test it against the same transaction/query semantics.
4. Expand concurrency and corruption stress coverage for B+Tree roots, snapshot history, lock contention, compaction, and recovery.
5. Add fuzz targets and storage-format golden fixtures, then reproducible benchmarks after correctness and crash recovery are established.

## Explicitly out of scope

Clustering, sharding, consensus, replication, cloud service, MongoDB/PostgreSQL wire protocols, full SQL, ORM, GUI, admin dashboard, Kubernetes operator, a custom programming language, and Tyvera-specific runtime dependencies.
