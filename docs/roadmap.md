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
- [ ] Persist index-entry deltas or page-based indexes; index entries are currently rebuilt from documents.
- [ ] Add a fuzzing harness and broader randomized index/recovery properties.
- [ ] Verify stress behavior under CI on Linux and Windows before making stability claims.

## Next engine wave

1. Fuzz malformed headers, record streams, transaction state machines, and nested document decoders.
2. Implement transactional index-entry persistence, then design page/block allocation and its migration path before adding on-disk B+Tree pages.
3. Expand concurrency and corruption stress coverage for snapshot history limits, lock contention, compaction, and recovery.
4. Specify and implement a narrow SQL parser adapter over the existing native query AST; keep SQL and native execution on one engine.
5. Add storage-format golden fixtures for V2/V3 and reproducible benchmarks after correctness and crash recovery are established.

## Explicitly out of scope

Clustering, sharding, consensus, replication, cloud service, MongoDB/PostgreSQL wire protocols, full SQL, ORM, GUI, admin dashboard, Kubernetes operator, a custom programming language, and Tyvera-specific runtime dependencies.
