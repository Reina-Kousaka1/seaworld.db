# Embedded API and future driver boundaries

SeaWorld currently exposes a Rust embedded library. This is an API boundary, not a second storage implementation: the native query API and any future SQL front end must build the same `Query`, use the same planner, and execute against the same `Database` state.

## Current ownership model

- `Database::open` owns the writable file handle, writer lock, recovered state, indexes, and MVCC history. The documented model permits one writer.
- `Database::open_reader` owns a fixed read-only view. It takes a shared file lock while replaying and then reads its immutable in-memory view.
- `Transaction` holds an exclusive mutable borrow of its `Database`. Staged changes are visible to that transaction; commit writes one transaction batch and rollback writes nothing.
- `Snapshot` is an owning, cloneable lease into the bounded MVCC history. It can outlive a borrow of `Database`, but snapshots are process-local and do not survive reopen.
- `Prepared Query` is a desired driver-level concept. No SQL prepared-statement or connection-pool API exists yet.

There is no async runtime requirement, connection pool, background server, or network side effect. File paths are supplied by the embedding application. The library does not create temporary database files for an in-memory mode; that mode is not implemented.

## Future language support

| Consumer | Intended boundary | Status |
| --- | --- | --- |
| Rust applications | Native Rust crate API | Current |
| Java | Future stable protocol or deliberately designed FFI | Not implemented |
| Tyvera | Future independent SeaWorld connector using a public API/protocol | Not implemented; no Tyvera dependency |
| Python | Future protocol client or reviewed C ABI | Not implemented |
| JavaScript / TypeScript | Future protocol client | Not implemented |
| C / C++ | Possible stable C ABI after ownership and error semantics are fixed | Not implemented |

No network or FFI protocol has been selected. Language bindings must translate to a stable SeaWorld request/query representation, not duplicate storage, recovery, transaction, or index logic.

## Future server mode

A future server should host this same engine and expose a versioned SeaWorld protocol. Embedded and server deployments must not fork the storage engine. Authentication, authorization, TLS, rate limiting, request and connection limits, protocol compatibility, and administrative behavior require a separate threat review before a network listener is added. No wire-protocol compatibility with MongoDB or PostgreSQL is promised.

The present crate exposes SQL V0 with positional `?` values through `Database::execute_sql`, but has no reusable prepared-statement object. Server and FFI interfaces remain unimplemented. See the [SQL subset](sql.md) and [roadmap](roadmap.md).
