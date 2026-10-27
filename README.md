# SeaWorld DB

> **Experimental software:** SeaWorld DB is an experiment in building an independent, document-native database engine with a planned SQL compatibility layer. It is not production-ready.

SeaWorld DB is a standalone project. It is not part of Tyvera, has no Tyvera runtime dependency, and is intended to serve applications written in Java, Python, JavaScript/TypeScript, Tyvera, and other languages through future connectors.

The engine is being built in Rust around its own document model, query representation, index and transaction layers, and `*.seaworld.db` file format. SQL is planned as an adapter over the same query engine. The project does not wrap or forward operations to SQLite, PostgreSQL, MongoDB, H2, or Apache Derby, and it copies no code from those projects.

## Status

Current development version: `0.0.1-dev`.

The initial milestone is a small embedded engine that writes its own versioned database file, persists collections and documents, and reads them after close and reopen. Rust reads and writes the version 1 format; a checked-in fixture produced by the former Java implementation and a byte comparison protect compatibility with the original format. The Java engine and its test suite were removed after the Rust CI compatibility gate passed. See [the roadmap](docs/roadmap.md) for implemented and planned work.

SeaWorld DB does not claim production readiness, full ACID compliance, a complete SQL implementation, distribution, or production security. Crash recovery and durability guarantees are limited to the documented behavior and tested environments.

## Architecture

Applications will use either a native SeaWorld API or a SQL compatibility layer. Both will translate into the same internal query representation and document engine. Storage details and the versioned file format are described in [architecture](docs/architecture.md) and [storage format](docs/storage-format.md).

## Build

The core targets stable Rust 1.89 or later. From the repository root, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --features crash-test -- -D warnings
cargo test --all-targets --features crash-test
cargo build --release
```

The only direct runtime dependency is `getrandom`, used to create the non-secret database UUID from the operating system random source. Dependency versions are locked in `Cargo.lock`. CI runs formatting, Clippy, tests, and a release build on Linux and Windows.

## Data files

Database files should use the `.seaworld.db` suffix. They contain user data and should not be committed to source control.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md). This repository retains its existing AGPL-3.0 license; the options considered are recorded in [docs/licensing.md](docs/licensing.md).
