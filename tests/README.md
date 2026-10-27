# Test layout

The Rust integration suite is `tests/engine.rs`, with subprocess crash tests in `tests/crash_recovery.rs`. `fixtures/java-v1-basic.fixture` is a checked-in database produced by the former Java v1 implementation; Rust opens it and checks that Rust-written records match its record bytes. The fixture remains as a permanent compatibility test after removing the Java engine and test suite.

The Rust suite also covers clean create/close/reopen, V1 compatibility and V2 migration, multi-operation commit and rollback, nested values, persistent single-field/unique indexes, native query planning and execution, index-vs-scan results, bounds, malformed headers/records, duplicate IDs, CRC32C, exclusive locking, repeatable recovery, and compacted-copy integrity. A parent test process kills the separate crash writer before, during, and after V2 transaction boundaries, then interrupts recovery to check idempotence.
