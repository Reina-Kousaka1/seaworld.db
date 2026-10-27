# SeaWorld DB architecture

## Purpose and boundaries

SeaWorld DB is an independent database experiment. Its engine has no Tyvera dependency and is not a fork, wrapper, or proxy for another database. MongoDB, relational systems, Derby, and H2 are conceptual references only.

The first release is embedded-first: an application opens a local `*.seaworld.db` file through a SeaWorld library. A future server may host the same engine behind a network protocol. Replication, clustering, and distributed storage are outside the current scope.

## Language decision: Rust stable

The engine core is implemented in stable Rust. `Cargo.toml` declares Rust 1.89 as its minimum version. Rust is the engine implementation language; future drivers can target Java, Python, JavaScript/TypeScript, Tyvera, and other hosts without changing that boundary.

V1 remains byte-for-byte supported. New databases use the explicitly versioned V3 header and transaction grammar; V2 remains readable and writable through an explicit constructor. V1 handles keep single-operation CRUD and can be copied to V2 or V3 through a non-overwriting migration path before using transactions or indexes.

| Area | C++ | Rust | Java 21 |
| --- | --- | --- | --- |
| Memory safety | RAII and modern ownership types help manage resources, but bounds and lifetime errors remain possible in unsafe code. | Ownership and borrowing prevent many memory errors at compile time without a garbage collector. | Managed memory prevents ordinary pointer misuse; heap use and garbage collection need to be considered for an embedded engine. |
| Performance | High control and predictable native allocation; maximum control also increases implementation risk. | Native code with low runtime overhead and compile-time safety checks. | JIT compilation can optimize hot paths; startup, object footprint, and GC behavior can affect some embedded workloads. No performance claim is made. |
| File I/O | Mature native APIs, but OS-specific details and error handling need care. | Standard file APIs expose explicit sync operations; platform semantics still matter. | NIO `FileChannel` supports reads, writes, locks, truncation, and forcing file updates. |
| Concurrency | Mature primitives with safety depending on ownership and synchronization discipline. | Ownership and `Send`/`Sync` checks catch classes of data-race mistakes at compile time. | Mature threads, synchronization, and executor APIs; mutable shared engine state still needs explicit locking. |
| Portability | Broad, but compiler and platform API differences must be managed. | Broad target support, with support guarantees varying by target tier. | JDK 21 provides a consistent runtime and file/network APIs on major desktop and server platforms. |
| Embedded use | Small native library and direct language bindings are possible. | Small native library and C ABI integration are possible; a Rust ABI is not a stable cross-language interface. | Simple in-process library for JVM applications; requires a JVM and has a larger runtime footprint. |
| Server use | Strong choice, with networking libraries selected separately. | Strong choice; networking and asynchronous runtimes commonly add dependencies. | Strong standard-library networking and mature server ecosystem. |
| Networking | Broad native ecosystem, with platform and library choices to maintain. | Standard TCP support is available; higher-level async servers commonly use crates. | `java.net` and NIO networking are part of the JDK, reducing dependencies for a later server. |
| Testing | Mature test frameworks, usually selected separately. | Built-in unit and integration testing support. | Built-in language tooling; the initial project uses a dependency-free test runner. |
| Crash recovery | Language does not supply database recovery; storage protocol and OS flush semantics determine it. | Same: the engine must design and test its recovery protocol. | Same: `FileChannel.force` is available, but the engine still must design and test recovery. |
| Maintenance | Large ecosystem and long history; manual memory hazards need rigorous review. | Strong safety defaults; steeper learning curve and a smaller pool of database-specific experience. | Familiar, mature ecosystem and easy access to JDK file/network APIs; memory footprint is the main embedded tradeoff. |

Rust is selected for the engine core because SeaWorld is intended to be a native embedded library usable from multiple host languages. Ownership and borrowing reduce memory-safety and data-race risks at compile time, while the standard file APIs support explicit synchronization and the language has no mandatory garbage-collected runtime. C++ offers similar native control but leaves more lifetime and bounds safety to conventions. Java remains a strong portable embedded/server option and was useful as the first reference implementation; its JVM footprint and native-embedding boundary are less aligned with this engine's multi-language library goal. This is an architectural choice, not a performance claim: SeaWorld has no benchmark results. The selection is independent of Tyvera, and any future foreign-language ABI should use a deliberate stable C boundary rather than Rust's native ABI.

The production crate has one direct runtime dependency: [`getrandom` 0.4.3](https://docs.rs/crate/getrandom/0.4.3), which asks the operating system for random bytes used only for the database UUID (the UUID is not a secret). Its license is MIT OR Apache-2.0. Its locked transitive dependencies are [`cfg-if` 1.0.5](https://docs.rs/crate/cfg-if/1.0.5), [`libc` 0.2.191](https://docs.rs/crate/libc/0.2.191), and [`r-efi` 6.0.0](https://docs.rs/crate/r-efi/6.0.0); the first two offer MIT OR Apache-2.0, while `r-efi` offers MIT OR Apache-2.0 OR LGPL-2.1-or-later. CRC32C is implemented in the format module to avoid a second runtime dependency. The exact resolved versions are recorded in `Cargo.lock`.

The comparison relies on the [Rust ownership](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html), [Rust concurrency](https://doc.rust-lang.org/book/ch16-00-concurrency.html), [Rust file API](https://doc.rust-lang.org/std/fs/struct.File.html), [Rust target support](https://doc.rust-lang.org/rustc/platform-support.html), [Rust C ABI](https://doc.rust-lang.org/reference/items/external-blocks.html), [C++ Core Guidelines](https://isocpp.github.io/CppCoreGuidelines/), and Java 21 [NIO](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/nio/channels/package-summary.html), [FileChannel](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/nio/channels/FileChannel.html), and [networking](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/net/package-summary.html) documentation. These documents describe language capabilities; the performance and maintenance judgments above are engineering assessments, not benchmark results.

## Layering

```text
Application
  ├── Native SeaWorld API ─┐
  └── SQL compatibility ───┤
                           v
                      Query Engine
                           |
                      Query Planner
                           |
                     Document Model
                           |
                   Index / Transactions
                           |
                      Storage Engine
                           |
                     *.seaworld.db
```

The query engine consumes SeaWorld's own typed query representation. Adapters translate native API calls or a future supported SQL subset into that representation. They do not implement independent storage paths. The SQL parser is not implemented.

## Modules

- **Storage engine:** owns file creation/opening, V1/V2/V3 versioned headers, append-only record blocks, checksums, bounded reads, `File::sync_all` durability calls, an exclusive writer sidecar lock plus shared/exclusive database-file locks, version-aware transaction recovery, and compacted copies without replacing the source.
- **Document model:** defines null, boolean, signed 64-bit integer, finite floating point, UTF-8 string, binary, array, and object values. Nested objects and arrays are ordinary values, not a SQL-only feature.
- **Collections:** group documents and persist collection metadata. Document IDs are stable, positive 64-bit identifiers within a database.
- **Index engine:** the primary ID map and single-field/unique secondary indexes are rebuilt from committed state on replay. Ordered in-memory entries support equality and range candidates. Persistent index entries/pages, compound indexes, and text indexes are future work.
- **Query engine/planner:** evaluates a typed native filter AST, chooses ID/index lookups or scans, and exposes a basic explain plan. It is independent of SQL.
- **Transactions/recovery:** V2/V3 mutations are grouped under begin/commit markers in the existing append log. Recovery applies complete committed batches and discards an uncommitted tail. V3 snapshots retain bounded in-memory document versions; no separate WAL or page-based recovery exists.
- **SQL interface:** a future adapter for a documented subset. It maps SQL null, missing fields, nested paths, arrays, and flexible schemas onto document/query semantics described in `query-engine.md`.
- **Server/driver:** a later transport boundary around the same engine. No cluster, replication, or wire-protocol compatibility is in scope.

## Concurrency and ownership

One exclusive writer handle and any fixed read-only handles are supported. The writer lock prevents a second cooperating writer; readers replay under a shared commit lock and then keep an in-memory view. V3 snapshots held by the writer retain a stable in-process view across commits. Lock enforcement is platform-dependent; another program may still modify the file outside SeaWorld's locking protocol. No isolation level beyond snapshot reads is promised. A future server can own one engine handle and schedule requests over a protocol without moving network concerns into storage code.

## Integrity and trust boundaries

Every persisted length is checked before allocation. The decoder limits record size, document size, nesting depth, collection-name size, field-name size, and element counts. UTF-8 decoding is strict. Header and record CRC32C checks detect accidental corruption; they are not authentication and do not protect against malicious modification. Invalid complete records fail open with a controlled error. Only an incomplete final record is recoverable, and its discarded byte count is exposed to the caller.

The engine never derives file paths from collection names or document values. The database path is supplied by the application. There is no encryption or authentication in Phase 1.
