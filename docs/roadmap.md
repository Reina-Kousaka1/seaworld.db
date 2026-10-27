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
- [x] Run Rust CI on Linux and Windows; retain the Java reference suite through the compatibility gate.

## Next engine wave

1. Add single-field and unique indexes only after query scan semantics are stable.
2. Specify transaction boundaries, rollback, and isolation before implementing multi-operation transactions.
3. Define a version migration tool and keep golden format fixtures before format stability.
4. Add a language-neutral query AST and native query API; only then add a small SQL parser adapter.
5. Measure reproducible benchmarks after correctness and crash recovery are established.

## Explicitly out of scope

Clustering, sharding, consensus, replication, cloud service, MongoDB/PostgreSQL wire protocols, full SQL, ORM, GUI, admin dashboard, Kubernetes operator, a custom programming language, and Tyvera-specific runtime dependencies.
