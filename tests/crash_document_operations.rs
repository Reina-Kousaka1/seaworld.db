// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
#![cfg(feature = "crash-test")]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use seaworld_db::{CompareOp, Database, Document, Error, FieldPath, Filter, Query, Value};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase {
    path: PathBuf,
    ready_path: PathBuf,
}

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seaworld-crash-document-{}-{id}.seaworld.db",
            std::process::id()
        ));
        let ready_path = path.with_extension("ready");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&ready_path);
        Self { path, ready_path }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.ready_path);
        let _ = fs::remove_file(format!("{}.writer.lock", self.path.display()));
    }
}

fn score_path() -> FieldPath {
    FieldPath::parse("score").unwrap()
}

fn item(key: &str, score: i64) -> Document {
    Document::empty()
        .with("key", Value::String(key.into()))
        .with("score", Value::Integer(score))
}

fn prepare(path: &PathBuf) {
    let mut database = Database::create(path).unwrap();
    database.create_collection("users").unwrap();
    database.insert("users", item("a", 1)).unwrap();
    database.insert("users", item("b", 2)).unwrap();
    database.insert("users", item("c", 3)).unwrap();
    database.create_index("users", "score").unwrap();
    database.create_unique_index("users", "key").unwrap();
    database.close().unwrap();
}

fn rows(database: &Database) -> Vec<(i64, Document)> {
    database
        .execute(&Query::new("users", Filter::All))
        .unwrap()
        .into_iter()
        .map(|row| (row.id.value(), (*row.document).clone()))
        .collect()
}

fn run_mode(path: &PathBuf, mode: &str) {
    let status = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
        .arg(path)
        .arg(mode)
        .status()
        .unwrap();
    assert!(status.success(), "{mode} exited with {status}");
}

fn crash_mode(temp: &TempDatabase, mode: &str, point: &str) {
    let _ = fs::remove_file(&temp.ready_path);
    let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
        .arg(&temp.path)
        .arg(mode)
        .env("SEAWORLD_CRASH_AT", point)
        .env("SEAWORLD_CRASH_READY_FILE", &temp.ready_path)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("{mode} exited before {point}: {status}");
        }
        if temp.ready_path.exists() {
            child.kill().unwrap();
            child.wait().unwrap();
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{mode} did not reach {point} within 10 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_index_matches_full_scan(database: &Database) {
    for (operator, value) in [
        (CompareOp::Eq, 1),
        (CompareOp::Eq, 2),
        (CompareOp::Eq, 3),
        (CompareOp::Eq, 4),
        (CompareOp::Eq, 11),
        (CompareOp::Eq, 12),
        (CompareOp::Eq, 13),
        (CompareOp::Gte, 2),
        (CompareOp::Lt, 12),
    ] {
        let filter = Filter::Compare {
            path: score_path(),
            op: operator,
            value: Value::Integer(value),
        };
        let indexed = Query::new("users", filter.clone());
        let scan = Query::new(
            "users",
            Filter::Not(Box::new(Filter::Not(Box::new(filter)))),
        );
        let indexed_ids = database
            .execute(&indexed)
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let scan_ids = database
            .execute(&scan)
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        assert_eq!(indexed_ids, scan_ids, "{operator:?} {value}");
    }
}

#[test]
fn document_operations_crash_only_publish_whole_committed_states() {
    let points = [
        "transaction_before_begin",
        "transaction_during_first_operation",
        "transaction_between_operations",
        "transaction_after_last_operation",
        "transaction_during_commit_record",
        "transaction_after_commit_write_before_sync",
        "transaction_after_sync_before_return",
        "transaction_after_publish",
    ];
    let modes = [
        "--bulk-update",
        "--bulk-delete",
        "--upsert",
        "--mixed-transaction",
    ];

    for mode in modes {
        let expected_file = TempDatabase::new();
        prepare(&expected_file.path);
        run_mode(&expected_file.path, mode);
        let expected_database = Database::open_reader(&expected_file.path).unwrap();
        let expected = rows(&expected_database);
        assert_index_matches_full_scan(&expected_database);
        expected_database.close().unwrap();

        for point in points {
            let temp = TempDatabase::new();
            prepare(&temp.path);
            let snapshot = Database::open_reader(&temp.path).unwrap();
            let before = rows(&snapshot);
            crash_mode(&temp, mode, point);
            assert_eq!(
                rows(&snapshot),
                before,
                "{mode} changed fixed reader at {point}"
            );
            snapshot.close().unwrap();

            let mut recovered = Database::open(&temp.path).unwrap();
            let actual = rows(&recovered);
            assert!(
                actual == before || actual == expected,
                "{mode} produced a partial state at {point}: {actual:?}"
            );
            recovered.check_integrity().unwrap();
            assert_index_matches_full_scan(&recovered);
            assert!(matches!(
                recovered.insert("users", item("a", 99)),
                Err(Error::ConstraintViolation(_))
            ));
            recovered.close().unwrap();

            let reopened = Database::open_reader(&temp.path).unwrap();
            assert_eq!(
                rows(&reopened),
                actual,
                "{mode} changed after reopen at {point}"
            );
            assert_index_matches_full_scan(&reopened);
        }
    }
}
