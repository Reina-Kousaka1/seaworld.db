# Test layout

The Rust integration suite is `tests/engine.rs`, with subprocess crash tests in `tests/crash_recovery.rs`. `fixtures/java-v1-basic.fixture` is a checked-in database produced by the former Java v1 implementation; Rust opens it and checks that Rust-written records match its record bytes. The fixture remains as a permanent compatibility test after removing the Java engine and test suite.

The Rust suite also covers clean create/close/reopen, collections, insert/read/update/delete, nested values, bounds, malformed headers/records, duplicate IDs, CRC32C, exclusive locking, repeatable recovery, and compacted-copy integrity. A parent test process kills the separate crash writer at the four boundaries listed in `docs/storage-format.md`.
