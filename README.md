# SeaWorld DB

> **Experimental database engine.** SeaWorld DB is an experiment in building an independent, document-native database engine with a planned SQL compatibility layer. It is not production-ready.

SeaWorld DB is a standalone project, independent of Tyvera, Tori, and Noélia. It has no language-runtime dependency and is intended to serve Java, Rust, Python, JavaScript/TypeScript, Tyvera, and other applications through future connectors.

The engine is implemented in Rust around its own document model, native query representation, transaction layer, storage format, and persistent single-field B+Tree indexes. New databases use version 4 of the `*.seaworld.db` file format; SeaWorld continues to read V1, V2, and V3. It does not wrap, proxy, or forward operations to SQLite, PostgreSQL, MongoDB, H2, Apache Derby, or another database.

## Current scope

Implemented features include:

- Embedded library API for create/open, collections, document CRUD, transactions, MVCC snapshots, and read-only views.
- Nested document values: null, booleans, integers, finite floats, strings, binary, arrays, and objects.
- One writer with concurrent fixed readers and stable in-process snapshots.
- Native typed filters and a planner for primary ID, secondary equality/range, and collection scans.
- V4 immutable, checksummed B+Tree pages with copy-on-write updates and transactional root publication in the same append-only file.
- Reopen of persisted V4 indexes without rebuilding their entries by scanning all documents.
- A bounded SQL V0 parser for SELECT, INSERT, UPDATE, DELETE, CREATE COLLECTION, and EXPLAIN SELECT; prepared `?` bindings lower into the native query/transaction engine.
- Process-crash harness and Linux/Windows CI.

The B+Tree currently supports scalar single-field keys. Old copy-on-write pages stay in the append log until V4 compaction. The embedded API exposes index metadata, a read-only integrity report, and safe no-overwrite backup/restore-to-new-file operations. A small shell provides metadata inspection, check, backup/restore commands, and the supported SQL subset. SQL remains deliberately limited to one comparison predicate per statement; it is not full SQL. In-memory storage, multi-writer transactions, network server, replication, and distributed storage are not implemented. See [SQL V0](docs/sql.md), the [driver boundaries](docs/drivers.md), the [roadmap](docs/roadmap.md), and [storage format](docs/storage-format.md).

SeaWorld does not claim production readiness, full ACID compliance, complete SQL compatibility, distribution, or production security. Process-kill tests do not prove power-loss durability. CRC32C checks detect accidental corruption; they do not authenticate a file.

## Architecture

Applications use one document-native engine. The native API and SQL V0 lower into the same query representation and planner. No separate SQL storage path is planned. See [architecture](docs/architecture.md), [persistent index storage](docs/index-storage.md), and [transactions](docs/transactions.md).

## Build and verify

The crate targets stable Rust 1.89 or later:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --features crash-test --locked -- -D warnings
cargo test --all-targets --features crash-test --locked
cargo build --release --locked
```

CI runs formatting, Clippy, tests, and a release build on Ubuntu and Windows. `getrandom` is the only direct runtime dependency and supplies random bytes for the non-secret database UUID. `proptest` is a test-only dependency for generated B+Tree operation sequences. See [third-party notices](THIRD_PARTY_NOTICES.md) for locked dependency licenses.

## Data files

Use the `.seaworld.db` suffix. Files contain application data and should not be committed to source control. V4 compaction writes a new file and refuses to overwrite an existing destination; the source remains unchanged.

The CLI is built as `seaworld`. `seaworld DATABASE.seaworld.db` opens or creates an embedded database shell. `seaworld check`, `seaworld verify`, `seaworld backup`, and `seaworld restore` provide read-only validation and never overwrite an existing destination. See [backup and restore](docs/backup.md) for identity, snapshot, and durability boundaries.

## Contributing, security, and license

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md). SeaWorld DB is published under **AGPL-3.0-only**. The license choice and its tradeoffs are recorded in [docs/licensing.md](docs/licensing.md).
