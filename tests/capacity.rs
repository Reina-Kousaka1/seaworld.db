// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
//! File-capacity regression tests (audit finding P1-1).
//!
//! The fast tests fill a database with large binary documents so the 128 MiB
//! file limit is reached with few writes. The `#[ignore]` tests reproduce the
//! realistic copy-on-write growth of indexed updates and inserts; CI runs them
//! as a separate capacity job with `--release -- --ignored`.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    Database, Document, DocumentId, Error, ErrorCode, Filter, MAX_DATABASE_BYTES, Query, Value,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("seaworld-capacity-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const LARGE: usize = 8 * 1024 * 1024;

fn large(tag: u8) -> Document {
    Document::empty()
        .with("tag", Value::Integer(i64::from(tag)))
        .with("blob", Value::Binary(vec![tag; LARGE]))
}

fn all_rows(database: &Database, collection: &str) -> Vec<(i64, Document)> {
    database
        .execute(&Query::new(collection, Filter::All))
        .unwrap()
        .into_iter()
        .map(|row| (row.id.value(), (*row.document).clone()))
        .collect()
}

/// Inserts large documents until the file limit rejects one; returns the error.
fn fill(database: &mut Database) -> Error {
    for tag in 0..=u8::MAX {
        if let Err(error) = database.insert("blobs", large(tag)) {
            return error;
        }
    }
    panic!("the file limit was never reached");
}

fn assert_capacity_rejection_keeps_handle_usable(mut database: Database, path: &PathBuf) {
    database.create_collection("blobs").unwrap();
    let error = fill(&mut database);
    assert!(error.is_capacity_exhausted(), "unexpected error: {error}");
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(!matches!(error, Error::Io(_) | Error::Integrity(_)));
    match error {
        Error::CapacityExhausted {
            file_bytes,
            requested_bytes,
            limit_bytes,
        } => {
            assert_eq!(limit_bytes, MAX_DATABASE_BYTES);
            assert!(file_bytes + requested_bytes > limit_bytes);
            assert_eq!(file_bytes, fs::metadata(path).unwrap().len());
        }
        other => panic!("expected CapacityExhausted, got {other:?}"),
    }

    // The rejected write appended nothing and did not poison the handle.
    let size = fs::metadata(path).unwrap().len();
    let rows = all_rows(&database, "blobs");
    assert!(!rows.is_empty());
    let retry = database.insert("blobs", large(200)).unwrap_err();
    assert!(retry.is_capacity_exhausted());
    assert_eq!(fs::metadata(path).unwrap().len(), size);
    database.check_integrity().unwrap();
    let usage = database.storage_usage().unwrap();
    assert_eq!(usage.file_bytes, size);
    assert_eq!(usage.limit_bytes, MAX_DATABASE_BYTES);
    assert_eq!(usage.remaining_bytes, MAX_DATABASE_BYTES - size);
    assert!(usage.compaction_recommended);

    // A write that still fits is accepted after the rejection.
    let small = database
        .insert("blobs", Document::empty().with("tag", Value::Integer(-1)))
        .unwrap();
    assert_eq!(
        database.get("blobs", small).unwrap(),
        Some(&Document::empty().with("tag", Value::Integer(-1)))
    );
    let expected = all_rows(&database, "blobs");
    database.close().unwrap();

    let reopened = Database::open(path).unwrap();
    assert_eq!(reopened.recovery_report().discarded_bytes(), 0);
    assert_eq!(all_rows(&reopened, "blobs"), expected);
    reopened.check_integrity().unwrap();
}

#[test]
fn capacity_rejection_is_distinct_and_keeps_v4_handle_usable() {
    let dir = TestDir::new();
    let path = dir.path("v4.seaworld.db");
    assert_capacity_rejection_keeps_handle_usable(Database::create(&path).unwrap(), &path);
}

#[test]
fn capacity_rejection_is_distinct_and_keeps_v2_handle_usable() {
    let dir = TestDir::new();
    let path = dir.path("v2.seaworld.db");
    assert_capacity_rejection_keeps_handle_usable(Database::create_v2(&path).unwrap(), &path);
}

#[test]
fn capacity_rejection_is_distinct_and_keeps_v1_handle_usable() {
    let dir = TestDir::new();
    let path = dir.path("v1.seaworld.db");
    assert_capacity_rejection_keeps_handle_usable(Database::create_v1(&path).unwrap(), &path);
}

#[test]
fn reader_on_full_file_stays_readable() {
    let dir = TestDir::new();
    let path = dir.path("full.seaworld.db");
    let mut database = Database::create(&path).unwrap();
    database.create_collection("blobs").unwrap();
    assert!(fill(&mut database).is_capacity_exhausted());
    let expected = all_rows(&database, "blobs");
    let reader = Database::open_reader(&path).unwrap();
    assert_eq!(all_rows(&reader, "blobs"), expected);
    reader.check_integrity().unwrap();
}

#[test]
fn index_creation_on_full_file_is_rejected_without_growth() {
    let dir = TestDir::new();
    let path = dir.path("index.seaworld.db");
    let mut database = Database::create(&path).unwrap();
    database.create_collection("blobs").unwrap();
    assert!(fill(&mut database).is_capacity_exhausted());
    // Consume the remaining space with progressively smaller documents.
    let mut next = 0_i64;
    for blob in [256 * 1024, 16 * 1024, 512] {
        loop {
            let document = Document::empty()
                .with("tag", Value::Integer(next))
                .with("blob", Value::Binary(vec![1; blob]));
            match database.insert("blobs", document) {
                Ok(_) => next += 1,
                Err(error) => {
                    assert!(error.is_capacity_exhausted(), "{error}");
                    break;
                }
            }
        }
    }
    let size = fs::metadata(&path).unwrap().len();
    let pages_before = database.storage_usage().unwrap().persistent_page_count;
    let error = database.create_index("blobs", "tag").unwrap_err();
    assert!(error.is_capacity_exhausted(), "{error}");
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
    assert!(database.indexes().unwrap().is_empty());
    assert_eq!(
        database.storage_usage().unwrap().persistent_page_count,
        pages_before
    );
    database.check_integrity().unwrap();
    database.close().unwrap();
    Database::open(&path).unwrap().check_integrity().unwrap();
}

#[test]
fn verified_compaction_after_capacity_error_restores_writes_and_keeps_data() {
    let dir = TestDir::new();
    let path = dir.path("source.seaworld.db");
    let target = dir.path("compacted.seaworld.db");
    let mut database = Database::create(&path).unwrap();
    database.create_collection("blobs").unwrap();
    database.create_index("blobs", "tag").unwrap();
    let kept = database.insert("blobs", large(1)).unwrap();
    // Replace the same large document until the file is full: the obsolete
    // versions are reclaimable, the live data is one document.
    let error = loop {
        if let Err(error) = database.update("blobs", kept, large(2)) {
            break error;
        }
    };
    assert!(error.is_capacity_exhausted());
    let source_bytes = fs::read(&path).unwrap();
    let expected = all_rows(&database, "blobs");
    let usage = database.storage_usage().unwrap();
    assert!(usage.compaction_recommended);
    assert!(usage.estimated_live_bytes < usage.file_bytes / 4);

    let report = database.compact_verified(&target).unwrap();
    assert_eq!(report.format_version, 4);
    assert_eq!(report.path, fs::canonicalize(&target).unwrap());
    assert_eq!(report.source_file_bytes, source_bytes.len() as u64);
    assert_eq!(
        report.compacted_file_bytes,
        fs::metadata(&target).unwrap().len()
    );
    assert!(report.compacted_file_bytes < report.source_file_bytes / 4);
    assert_eq!(report.integrity.document_count, 1);
    assert_eq!(report.integrity.index_count, 1);
    // The estimate is labelled as such but must be in a useful range.
    let estimate = usage.estimated_live_bytes as f64;
    let actual = report.compacted_file_bytes as f64;
    assert!(
        estimate > actual * 0.5 && estimate < actual * 2.0,
        "{estimate} vs {actual}"
    );
    // The source is untouched and still open.
    assert_eq!(fs::read(&path).unwrap(), source_bytes);
    assert_eq!(all_rows(&database, "blobs"), expected);
    database.close().unwrap();

    // Controlled switch: the caller opens the verified copy explicitly.
    let mut switched = Database::open(&report.path).unwrap();
    assert_eq!(all_rows(&switched, "blobs"), expected);
    assert!(switched.update("blobs", kept, large(3)).unwrap());
    switched.insert("blobs", large(4)).unwrap();
    switched.check_integrity().unwrap();
    switched.close().unwrap();
    // The original remains openable and unchanged.
    let original = Database::open_reader(&path).unwrap();
    assert_eq!(all_rows(&original, "blobs"), expected);
}

#[test]
fn storage_usage_separates_physical_size_from_live_estimate() {
    let dir = TestDir::new();
    let path = dir.path("usage.seaworld.db");
    let target = dir.path("usage-compacted.seaworld.db");
    let mut database = Database::create(&path).unwrap();
    database.create_collection("users").unwrap();
    database.create_index("users", "score").unwrap();
    let fresh = database.storage_usage().unwrap();
    assert!(!fresh.compaction_recommended);
    assert_eq!(fresh.file_bytes, fs::metadata(&path).unwrap().len());
    assert!(fresh.estimated_live_bytes <= fresh.file_bytes);

    let mut ids = Vec::new();
    for score in 0..50 {
        ids.push(
            database
                .insert(
                    "users",
                    Document::empty().with("score", Value::Integer(score)),
                )
                .unwrap(),
        );
    }
    for round in 0..20 {
        for id in &ids {
            database
                .update(
                    "users",
                    *id,
                    Document::empty().with("score", Value::Integer(1_000 + round)),
                )
                .unwrap();
        }
    }
    let usage = database.storage_usage().unwrap();
    assert_eq!(usage.file_bytes, fs::metadata(&path).unwrap().len());
    assert!(usage.persistent_page_count > usage.reachable_page_count);
    assert!(usage.reachable_page_count >= 1);
    assert_eq!(
        usage.estimated_reclaimable_bytes,
        usage.file_bytes - usage.estimated_live_bytes
    );
    assert!(usage.estimated_live_bytes < usage.file_bytes / 4);
    let report = database.compact_verified(&target).unwrap();
    let estimate = usage.estimated_live_bytes as f64;
    let actual = report.compacted_file_bytes as f64;
    assert!(
        estimate > actual * 0.5 && estimate < actual * 2.0,
        "{estimate} vs {actual}"
    );
}

fn indexed_database(path: &PathBuf, documents: i64) -> (Database, Vec<DocumentId>) {
    let mut database = Database::create(path).unwrap();
    database.create_collection("eco").unwrap();
    database.create_index("eco", "balance").unwrap();
    let ids = (0..documents)
        .map(|balance| {
            database
                .insert(
                    "eco",
                    Document::empty().with("balance", Value::Integer(balance)),
                )
                .unwrap()
        })
        .collect();
    (database, ids)
}

fn finish_long_run(database: Database, path: &PathBuf, target: &PathBuf, writes: u64) {
    assert!(writes > 1_000, "limit reached after only {writes} writes");
    let file_bytes = fs::metadata(path).unwrap().len();
    assert!(file_bytes > MAX_DATABASE_BYTES - 1024 * 1024);
    let expected = all_rows(&database, "eco");
    database.check_integrity().unwrap();
    let report = database.compact_verified(target).unwrap();
    assert!(report.compacted_file_bytes < 16 * 1024 * 1024);
    database.close().unwrap();
    let mut switched = Database::open(target).unwrap();
    assert_eq!(all_rows(&switched, "eco"), expected);
    for balance in 0..100 {
        switched
            .insert(
                "eco",
                Document::empty().with("balance", Value::Integer(-balance)),
            )
            .unwrap();
    }
    switched.check_integrity().unwrap();
    eprintln!(
        "capacity: {writes} writes filled {file_bytes} bytes; compacted to {} bytes",
        report.compacted_file_bytes
    );
}

#[test]
#[ignore = "long capacity test; run with --release -- --ignored"]
fn long_indexed_updates_reach_limit_cleanly_and_recover_by_compaction() {
    let dir = TestDir::new();
    let path = dir.path("updates.seaworld.db");
    let target = dir.path("updates-compacted.seaworld.db");
    let (mut database, ids) = indexed_database(&path, 1_000);
    let mut updates = 0_u64;
    let error = loop {
        let id = ids[(updates as usize) % ids.len()];
        let document =
            Document::empty().with("balance", Value::Integer(1_000_000 + updates as i64));
        match database.update("eco", id, document) {
            Ok(updated) => {
                assert!(updated);
                updates += 1;
            }
            Err(error) => break error,
        }
    };
    assert!(error.is_capacity_exhausted(), "{error}");
    finish_long_run(database, &path, &target, updates);
}

#[test]
#[ignore = "long capacity test; run with --release -- --ignored"]
fn long_indexed_inserts_reach_limit_cleanly_and_recover_by_compaction() {
    let dir = TestDir::new();
    let path = dir.path("inserts.seaworld.db");
    let target = dir.path("inserts-compacted.seaworld.db");
    let (mut database, _) = indexed_database(&path, 0);
    let mut inserts = 0_u64;
    let error = loop {
        let document = Document::empty().with("balance", Value::Integer(inserts as i64));
        match database.insert("eco", document) {
            Ok(_) => inserts += 1,
            Err(error) => break error,
        }
    };
    assert!(error.is_capacity_exhausted(), "{error}");
    finish_long_run(database, &path, &target, inserts);
}
