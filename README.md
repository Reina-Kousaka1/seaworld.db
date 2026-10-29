# SeaWorld DB

[![Release](https://img.shields.io/github/v/release/Reina-Kousaka1/seaworld.db?display_name=tag&sort=semver)](https://github.com/Reina-Kousaka1/seaworld.db/releases/tag/v1.0.0)
[![CI](https://github.com/Reina-Kousaka1/seaworld.db/actions/workflows/ci.yml/badge.svg)](https://github.com/Reina-Kousaka1/seaworld.db/actions/workflows/ci.yml)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange)](https://www.rust-lang.org/)
[![License: AGPL-3.0-only](https://img.shields.io/badge/License-AGPL--3.0--only-blue)](LICENSE)

SeaWorld DB is an embedded, document-oriented database written in Rust. V1
provides a document-native storage engine, transactions, MVCC snapshots,
single-field indexes, crash recovery, backup/restore, and a stable
transport-neutral public API.

It is an independent database project. SeaWorld uses its own document,
transaction, recovery, and on-disk storage layers; it does not wrap, proxy, or
embed SQLite, PostgreSQL, MongoDB, H2, Derby, or WiredTiger.

## Current release

**SeaWorld DB v1.0.0** is the first stable package and public Rust API release.

| Area | V1.0.0 |
| --- | --- |
| Package | `seaworld-db` `1.0.0` |
| Public API | `1.0.0` |
| Current on-disk format | V4 |
| Readable on-disk formats | V1–V4 |
| License | [AGPL-3.0-only](LICENSE) |

Package, public API, and on-disk format are separate version spaces. Their
initial package/API values happen to match; future format and API changes are
versioned independently. The checked-in Java-generated V1 fixture remains part
of the compatibility contract.

## What SeaWorld is

SeaWorld V1 is an embedded/local engine: an application links the Rust library
and opens a `*.seaworld.db` file directly. It has no required server process,
network listener, telemetry, cloud service, or language-runtime dependency.

The primary model is a collection of flexible documents. SQL V0 is a limited
access layer that lowers into the same native query and transaction engine; it
does not define the storage model and is not full SQL compatibility.

## V1 features

### Documents and collections

- Named collections with stable collection and document identifiers.
- Documents containing `null`, booleans, signed 64-bit integers, finite 64-bit
  floats, UTF-8 strings, binary bytes, arrays, and nested objects.
- Dotted field paths such as `profile.level`; paths traverse nested objects, not
  array elements.
- Flexible schemas: missing fields are distinct from explicit `null`.

### Native queries

- Primary ID lookup and typed equality/range comparisons.
- `Exists`, `IN`, `NOT IN`, `ArrayContains`, `AND`, `OR`, and `NOT` filters.
- Deterministic ordering, optional sort, limit, offset, and projections.
- Projections preserve explicit `null`, omit missing paths, and return arrays
  as whole values.
- The planner can use ID, secondary equality, or secondary range candidates;
  every index candidate is rechecked against the complete filter.

`ArrayContains` compares direct array elements. It does not flatten nested
arrays or introduce multikey indexes.

### Updates and bulk operations

- Ordered `Set`, `Unset`, `Increment`, and `Decrement` field operations.
- `update_one`, `update_many`, `delete_one`, `delete_many`, and `upsert_one`.
- Bulk operations run in one transaction: if an operation or commit fails, no
  partial successful bulk result is published.
- An upsert updates the first selected document or inserts an explicit base
  document after applying field operations; it does not derive inserted fields
  from the filter.

### Transactions and snapshots

- Explicit begin, commit, rollback, and read-your-own-writes in a transaction.
- One writer and bounded concurrent readers.
- V3/V4 owning MVCC snapshots and fixed read-only readers retain a stable
  committed view across later inserts, deletes, and predicate changes.
- A later reader sees later commits; an older snapshot does not gain phantoms.
- Transactions are bounded to 10,000 operations and 64 MiB of encoded operation
  data. They are not a Serializable or predicate-locking implementation.

### Indexes

- Built-in primary document-ID lookup.
- Persistent single-field secondary and unique indexes for V2–V4 files.
- Equality and ordered range candidates for indexable scalar values.
- Nested object paths, explicit `null`, booleans, integers, finite floats, and
  strings can be indexed. Missing fields have no index entry.
- V4 stores checksummed copy-on-write B+Tree pages and loads persisted index
  entries from published roots on reopen. V2/V3 rebuild index entries from
  committed documents on open.

Compound, text, geospatial, and multikey/array-element indexes are not part of
V1.

## Architecture

```text
Application
    |
    v
SeaWorld Public API / Native API / SQL V0
    |
    v
Document, Query, and Transaction Layer
    |
    v
MVCC, Index, and Recovery Layer
    |
    v
SeaWorld Storage Engine
    |
    v
*.seaworld.db
```

The public contract exposes owned values and typed errors, not storage pages,
cursors, append-log records, planner internals, or B+Tree handles. The current
Rust `Database` is the sole V1 backend.

## Quick start

Add SeaWorld to a Rust project:

```toml
[dependencies]
seaworld-db = "1.0.0"
```

Create a V4 database, insert a document, query it, apply a field update, and
commit a transaction:

```rust
use seaworld_db::{
    CompareOp, Database, Document, DocumentOperation, FieldPath, Filter, Query, Value,
};

fn main() -> seaworld_db::Result<()> {
    let mut db = Database::create("app.seaworld.db")?;
    db.create_collection("users")?;

    let id = db.insert(
        "users",
        Document::empty()
            .with("username", Value::String("Charlotte".into()))
            .with("credits", Value::Integer(1_250))
            .with(
                "profile",
                Value::object([("level".into(), Value::Integer(17))]),
            ),
    )?;

    let credits = FieldPath::parse("credits")?;
    let query = Query::new(
        "users",
        Filter::Compare {
            path: credits.clone(),
            op: CompareOp::Gte,
            value: Value::Integer(1_000),
        },
    );
    let matching_ids = db
        .execute(&query)?
        .into_iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert_eq!(matching_ids, vec![id]);

    db.update_one(
        &Query::new("users", Filter::IdEq(id)),
        &[DocumentOperation::Increment(credits, Value::Integer(250))],
    )?;

    let mut transaction = db.begin_transaction()?;
    transaction.update_fields(
        "users",
        id,
        &[DocumentOperation::Set(
            FieldPath::parse("profile.level")?,
            Value::Integer(18),
        )],
    )?;
    transaction.commit()?;

    db.close()
}
```

Use `Database::open` for a writable existing database and
`Database::open_reader` for a fixed read-only view. The example uses the same
API shapes exercised by the V1 document-operation tests.

## Public API stability

The **Public API 1.0.0** contract is stable throughout API major version 1.
`PublicEngine`, `PublicTransaction`, `ApiInfo`, `Capabilities`, `EngineHealth`,
`RequestId`, `OperationContext`, `PublicQueryRow`, `PublicRowValue`,
`ErrorCode`, and `API_VERSION` form the backend-neutral boundary. Their document
and query inputs use the documented `Document`, `Value`, `DocumentId`,
`CollectionId`, `FieldPath`, `Query`, `Filter`, `Projection`, and
`DocumentOperation` types.

Compatible 1.x releases may add documented methods, optional capabilities,
error codes, or fields on non-exhaustive public result types. Removing or
renaming a public method or capability, changing a stable error-code meaning,
or changing documented result semantics requires API major version 2.

## Errors, capabilities, and request IDs

`Error::code()` returns the stable machine-readable `ErrorCode` category.
Applications must not parse `Error` display text or Rust-local diagnostic
payloads. Relevant categories include invalid arguments, malformed documents,
resource limits, not found, conflicts, unique constraints, transaction state or
conflict, unsupported operation/capability, incompatible or unknown format,
corruption/integrity failure, recovery required, I/O, and internal invariants.
Unknown future error codes require a client fallback path.

Use `Capabilities::supports(name)` to distinguish:

- `Some(true)`: known and supported;
- `Some(false)`: known and unsupported;
- `None`: unknown future capability.

V1 guarantees native queries, projections, direct `ArrayContains`, field
updates, and transactions. It explicitly reports durable request deduplication
as unsupported.

`RequestId` and `OperationContext` are correlation metadata only. They do not
provide durable idempotency, write deduplication, exactly-once execution, or a
safe retry after an outcome-ambiguous commit.

## Persistence, recovery, and crash qualification

SeaWorld stores a custom, checksummed append-only record log. V4 adds immutable
checksummed B+Tree pages and transactionally published index roots in the same
file. A V2–V4 commit writes a complete transaction batch, synchronizes it with
`File::sync_all`, then publishes the in-memory state. A complete transaction
marker is the recovery decision point.

On writable open, an incomplete final transaction tail is discarded from its
begin record; a read-only open reports `RecoveryRequired` instead. Complete
checksum failures and semantically invalid transaction sequences fail closed.
Reopen validates the format and index/document consistency.

V1 qualification includes 32 semantic process-crash cases: `update_many`,
`delete_many`, `upsert_one`, and mixed insert/update/delete transactions are
interrupted at eight append/commit/publish boundaries. After reopening, each
case must show either the complete prior state or the complete committed state,
never a partial bulk mutation. The cases also check integrity, snapshot
stability, unique-index preservation, and indexed results against a forced full
scan.

This is process-crash qualification, not a simulation of every OS crash, power
loss, controller-cache, device, or filesystem failure.

## Fuzz, property, and differential testing

The V1 gate includes bounded generated coverage for arbitrary database bytes,
random tails after committed data, and checksummed-but-semantically-invalid
transaction sequences. It also includes generated document-operation/reference
model tests, B+Tree mutation sequences, index-versus-full-scan differentials,
snapshot/reopen checks, bulk operations, and upserts.

These tests broaden parser and recovery confidence; they are not a proof of
exhaustive input coverage or formal verification.

## Backup, restore, and compaction

`Database::backup` writes a validated compacted V4 copy of a committed view to
a new path. `Database::verify_backup` performs read-only structural validation.
`Database::restore_backup` validates a source and writes a validated V4 copy to
a new target. Backup, restore, and compaction do not overwrite an existing
target.

Copies preserve the database UUID, live collection/document IDs, index
definitions, and committed document-ID high-water mark. V3/V4 commit-version
watermarks never move backwards, although compaction can advance a watermark
while reconstructing a copy. Backup sources may be V1–V4; backup and restore
output V4.

## CLI

The `seaworld` binary opens an interactive embedded shell:

```sh
seaworld app.seaworld.db
```

Its public file-management commands are:

```sh
seaworld check DATABASE.seaworld.db
seaworld verify BACKUP.seaworld.db
seaworld backup SOURCE.seaworld.db NEW_BACKUP.seaworld.db
seaworld restore BACKUP.seaworld.db NEW_DATABASE.seaworld.db
```

The shell provides `.help`, `.open`, `.collections`, `.indexes`, `.schema`,
`.info`, `.check`, and `.quit`, plus the documented SQL V0 subset. No `compact`
CLI command is provided in V1.

## Compatibility, platforms, and durability

| Area | V1.0.0 status |
| --- | --- |
| Package | `1.0.0` |
| Public API | `1.0.0` |
| Current disk format | V4 |
| Readable disk formats | V1–V4 |
| Java V1 fixture | Supported compatibility fixture |
| Ubuntu 24.04 x86_64 | CI-qualified |
| Windows latest x86_64 | CI-qualified |
| ARM64 / Raspberry Pi | Not V1-qualified |

SeaWorld calls `File::sync_all` before reporting a successful commit and uses
cooperating file locks plus same-directory no-overwrite publication for copies.
That establishes the documented process-crash boundary only. It is not a
universal guarantee against OS crashes, power cuts, controller write-cache
loss, dishonest storage devices, or arbitrary filesystem behavior.

## Resource limits

V1 enforces bounds before accepting or decoding data, including:

- 16 MiB encoded documents, 17 MiB records, and 128 MiB database files;
- 64 nesting levels, 100,000 values per container, 100,000 active documents,
  and 10,000 collections;
- 10,000 transaction operations and 64 MiB transaction payloads;
- query limit/offset up to 100,000, filter depth 64, and 1,024 filter nodes;
- field paths up to 64 segments, projections up to 1,024 paths, and a 128 MiB
  projected-result accounting budget.

See the [compatibility contract](docs/compatibility-contract.md) and
[document model](docs/document-model.md) for the full boundary and caveats.

## Known V1 boundaries

- No universal power-loss or filesystem durability guarantee.
- ARM64/Raspberry Pi, macOS, network filesystems, removable storage, and
  container volumes are not V1-qualified platforms.
- One writer only; SeaWorld does not promise Serializable isolation or predicate
  locking.
- No durable external request-ID deduplication or exactly-once semantics.
- No multikey, compound, text, or geospatial indexes.
- SQL V0 is intentionally limited; it is not full SQL compatibility.
- No in-memory backend, network server, replication, sharding, or distributed
  consensus in V1.

## Future architecture

Post-V1 work may evaluate additional drivers, a transport service, ARM64
qualification, multikey indexes, stronger isolation, durable external
idempotency, and dedicated fuzzing targets.

WiredTiger is **not** part of SeaWorld DB v1.0.0: it is not activated,
integrated, or required. Its possible role as a future storage/backend or
failover boundary remains an architecture decision; the current SeaWorld Rust
engine and V1–V4 readers remain authoritative.

## Development and V1 quality gate

SeaWorld targets stable Rust 1.89 or later. Run the same local checks as CI:

```sh
cargo fmt --all -- --check
git diff --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --features crash-test --locked -- -D warnings
cargo test --all-targets --features crash-test --locked
cargo build --release --locked
```

The v1.0.0 release gate ran these checks on Ubuntu and Windows. Each platform
completed 105 tests with zero failures, including the generated tests and all
32 semantic crash cases. See the [final release-gate run](https://github.com/Reina-Kousaka1/seaworld.db/actions/runs/40876231817).

## Documentation

- [Public API V1 contract](docs/public-api.md)
- [Document model and update semantics](docs/document-model.md)
- [Native query engine](docs/query-engine.md)
- [Transactions and recovery](docs/transactions.md)
- [MVCC snapshots and reader/writer model](docs/mvcc.md)
- [Indexes](docs/indexes.md) and [persistent index storage](docs/index-storage.md)
- [Backup and restore](docs/backup.md)
- [Storage format](docs/storage-format.md)
- [V1 compatibility contract and release gate](docs/compatibility-contract.md)
- [Platform and durability qualification](docs/platform-support.md)
- [Security model](docs/security-model.md)
- [WiredTiger assessment](docs/wiredtiger-assessment.md)
- [Driver boundaries](docs/drivers.md) and [roadmap](docs/roadmap.md)

## Contributing, security, and license

Contributions are governed by [CONTRIBUTING.md](CONTRIBUTING.md). For security
reports, see [SECURITY.md](SECURITY.md). SeaWorld DB is licensed under
[AGPL-3.0-only](LICENSE); see [licensing notes](docs/licensing.md) and
[third-party notices](THIRD_PARTY_NOTICES.md).
