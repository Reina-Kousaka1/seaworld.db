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

## Next engine wave

1. Add update/delete records and verify replay, duplicate-ID handling, and primary-ID lookup.
2. Build subprocess crash injection around append and force boundaries; verify recovery and repeated recovery.
3. Add single-field and unique indexes only after scan semantics are stable.
4. Specify transaction boundaries, rollback, and isolation before implementing multi-operation transactions.
5. Define migration rules and golden file-format fixtures before format stability.
6. Add a language-neutral query AST and native query API; only then add a small SQL parser adapter.
7. Measure reproducible benchmarks after correctness and crash recovery are established.

## Explicitly out of scope

Clustering, sharding, consensus, replication, cloud service, MongoDB/PostgreSQL wire protocols, full SQL, ORM, GUI, admin dashboard, Kubernetes operator, a custom programming language, and Tyvera-specific runtime dependencies.
