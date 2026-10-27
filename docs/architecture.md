# SeaWorld DB architecture

## Purpose and boundaries

SeaWorld DB is an independent database experiment. Its engine has no Tyvera dependency and is not a fork, wrapper, or proxy for another database. MongoDB, relational systems, Derby, and H2 are conceptual references only.

The first release is embedded-first: an application opens a local `*.seaworld.db` file through a SeaWorld library. A future server may host the same engine behind a network protocol. Replication, clustering, and distributed storage are outside the current scope.

## Language decision: Java 21

The core is implemented in Java 21 using only the JDK. This is a deliberate choice for this small, portable, embedded-first foundation, not a commitment that every future driver must use Java.

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

Rust's ownership model and native library boundary make it the strongest alternative for a low-level, multi-language engine. C++ offers comparable control with more manual safety risk. Java is selected because an installed JDK 21 makes the first real file format and its tests immediately verifiable, and because its embedded library and future server can share one implementation without third-party runtime dependencies. This keeps Phase 0 focused on correctness and file-format behavior. If measured memory use, startup time, or native integration later makes the JVM unsuitable, revisit the language before stabilizing the file format. This choice is independent of Tyvera.

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

The query engine consumes SeaWorld's own query representation. Adapters translate native API calls or the supported SQL subset into that representation. They do not implement independent storage paths.

## Modules

- **Storage engine:** owns file creation/opening, the versioned header, append-only record blocks, checksums, bounded reads, durability calls, and recovery of an incomplete tail.
- **Document model:** defines null, boolean, signed 64-bit integer, finite floating point, UTF-8 string, binary, array, and object values. Nested objects and arrays are ordinary values, not a SQL-only feature.
- **Collections:** group documents and persist collection metadata. Document IDs are stable, positive 64-bit identifiers within a database.
- **Index engine:** the first in-memory primary ID map is rebuilt by replaying the file. Single-field and unique indexes are future work.
- **Query engine/planner:** will evaluate a language-neutral query AST and choose scans or indexes. Neither is coupled to SQL.
- **Transactions/recovery:** the first milestone has individually framed durable operations, not multi-operation transactions. A later design will add explicit transaction boundaries and a tested recovery protocol.
- **SQL interface:** a future adapter for a documented subset. It maps SQL null, missing fields, nested paths, arrays, and flexible schemas onto document/query semantics described in `query-engine.md`.
- **Server/driver:** a later transport boundary around the same engine. No cluster, replication, or wire-protocol compatibility is in scope.

## Concurrency and ownership

The first implementation serializes mutations within a database handle and takes an exclusive OS file lock so cooperating SeaWorld handles do not write the same file concurrently. File locks may be advisory; another program that ignores the lock can still modify the file. The engine does not offer concurrent readers, MVCC, or isolation levels. A future server can own one engine handle and schedule requests over a protocol without moving network concerns into storage code.

## Integrity and trust boundaries

Every persisted length is checked before allocation. The decoder limits record size, document size, nesting depth, collection-name size, field-name size, and element counts. UTF-8 decoding is strict. Header and record CRC32C checks detect accidental corruption; they are not authentication and do not protect against malicious modification. Invalid complete records fail open with a controlled error. Only an incomplete final record is recoverable, and its discarded byte count is exposed to the caller.

The engine never derives file paths from collection names or document values. The database path is supplied by the application. There is no encryption or authentication in Phase 0.
