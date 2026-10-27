# Test layout

The Rust integration suite is `tests/engine.rs`, with subprocess crash tests in `tests/crash_recovery.rs`. `fixtures/java-v1-basic.fixture` is a checked-in database produced by the original Java v1 implementation; Rust opens it and checks that Rust-written records match its Java-generated record bytes. During the migration compatibility window, `src/test/java/org/seaworlddb/EngineTests.java` remains as an independent reference suite.

The Rust suite also covers clean create/close/reopen, collections, insert/read/update/delete, nested values, bounds, malformed headers/records, duplicate IDs, CRC32C, exclusive locking, and repeatable recovery. A parent test process kills the separate crash writer at the four boundaries listed in `docs/storage-format.md`.
