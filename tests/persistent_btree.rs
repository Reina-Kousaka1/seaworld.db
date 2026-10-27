// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use seaworld_db::{CompareOp, Database, Document, DocumentId, FieldPath, Filter, Query, Value};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase {
    path: PathBuf,
    ready_path: PathBuf,
}

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seaworld-btree-{}-{id}.seaworld.db",
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
    }
}

fn query_for(operator: CompareOp, value: i64) -> Query {
    Query::new(
        "users",
        Filter::Compare {
            path: FieldPath::parse("score").unwrap(),
            op: operator,
            value: Value::Integer(value),
        },
    )
}

fn assert_index_matches_scan(database: &Database, operator: CompareOp, value: i64) {
    let query = query_for(operator, value);
    let indexed: Vec<_> = database
        .execute(&query)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    let mut scanned: Vec<_> = database
        .document_ids("users")
        .unwrap()
        .into_iter()
        .filter(|id| {
            let score = match database
                .get("users", *id)
                .unwrap()
                .unwrap()
                .get("score")
                .unwrap()
            {
                Value::Integer(score) => *score,
                _ => panic!("test document has a non-integer score"),
            };
            match operator {
                CompareOp::Eq => score == value,
                CompareOp::Lt => score < value,
                CompareOp::Lte => score <= value,
                CompareOp::Gt => score > value,
                CompareOp::Gte => score >= value,
                CompareOp::Ne => score != value,
            }
        })
        .collect();
    scanned.sort_unstable();
    assert_eq!(indexed, scanned);
}

#[test]
fn persistent_range_index_reopens_and_tracks_committed_mutations() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.path).unwrap();
    assert_eq!(database.format_version(), 4);
    database.create_collection("users").unwrap();
    let mut transaction = database.begin_transaction().unwrap();
    for ordinal in 0..700_i64 {
        transaction
            .insert(
                "users",
                Document::empty().with("score", Value::Integer((ordinal * 73) % 503)),
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    database.create_index("users", "score").unwrap();
    database.close().unwrap();

    let mut database = Database::open(&temp.path).unwrap();
    assert_index_matches_scan(&database, CompareOp::Eq, 123);
    assert_index_matches_scan(&database, CompareOp::Lt, 100);
    assert_index_matches_scan(&database, CompareOp::Lte, 100);
    assert_index_matches_scan(&database, CompareOp::Gt, 400);
    assert_index_matches_scan(&database, CompareOp::Gte, 400);

    let id_one = DocumentId::new(1).unwrap();
    let id_two = DocumentId::new(2).unwrap();
    let id_three = DocumentId::new(3).unwrap();
    {
        let mut transaction = database.begin_transaction().unwrap();
        transaction
            .update(
                "users",
                id_one,
                Document::empty().with("score", Value::Integer(900)),
            )
            .unwrap();
        transaction.delete("users", id_two).unwrap();
        transaction
            .update(
                "users",
                id_three,
                Document::empty().with("score", Value::Integer(901)),
            )
            .unwrap();
        transaction.rollback();
    }
    assert_index_matches_scan(&database, CompareOp::Gte, 900);

    let old_score = database
        .get("users", id_one)
        .unwrap()
        .unwrap()
        .get("score")
        .unwrap()
        .clone();
    database
        .update(
            "users",
            id_one,
            Document::empty().with("score", Value::Integer(900)),
        )
        .unwrap();
    database.delete("users", id_two).unwrap();
    assert_ne!(
        database.get("users", id_one).unwrap().unwrap().get("score"),
        Some(&old_score)
    );
    assert_index_matches_scan(&database, CompareOp::Gte, 900);
    database.close().unwrap();

    let database = Database::open(&temp.path).unwrap();
    assert_index_matches_scan(&database, CompareOp::Eq, 123);
    assert_index_matches_scan(&database, CompareOp::Gte, 900);
}

#[test]
fn integrity_report_and_backup_restore_preserve_a_consistent_indexed_state() {
    let source = TempDatabase::new();
    let backup = TempDatabase::new();
    let restored = TempDatabase::new();
    let mut database = Database::create(&source.path).unwrap();
    database.create_collection("users").unwrap();
    for value in 0..96_i64 {
        database
            .insert(
                "users",
                Document::empty().with("score", Value::Integer(value)),
            )
            .unwrap();
    }
    database.create_index("users", "score").unwrap();

    let report = database.check_integrity().unwrap();
    assert_eq!(report.format_version, 4);
    assert_eq!(report.collection_count, 1);
    assert_eq!(report.document_count, 96);
    assert_eq!(report.index_count, 1);
    assert!(report.persistent_page_count > 0);
    let indexes = database.indexes().unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].collection, "users");
    assert_eq!(indexes[0].path.to_string(), "score");
    assert!(indexes[0].root_page_id.is_some());

    database.backup(&backup.path).unwrap();
    assert!(matches!(
        database.backup(&backup.path),
        Err(seaworld_db::Error::Duplicate(_))
    ));
    database.close().unwrap();

    let backup_db = Database::open_reader(&backup.path).unwrap();
    assert_eq!(backup_db.check_integrity().unwrap().document_count, 96);
    assert_index_matches_scan(&backup_db, CompareOp::Gte, 70);
    backup_db.close().unwrap();

    Database::restore_backup(&backup.path, &restored.path).unwrap();
    let restored_db = Database::open(&restored.path).unwrap();
    assert_eq!(restored_db.check_integrity().unwrap().index_count, 1);
    assert_index_matches_scan(&restored_db, CompareOp::Eq, 42);
}

fn prepare_indexed_database(path: &PathBuf) {
    let mut database = Database::create(path).unwrap();
    database.create_collection("users").unwrap();
    database
        .insert("users", Document::empty().with("score", Value::Integer(1)))
        .unwrap();
    database.create_index("users", "score").unwrap();
    database.close().unwrap();
}

fn kill_index_writer(temp: &TempDatabase, point: &str) {
    let _ = fs::remove_file(&temp.ready_path);
    let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
        .arg(&temp.path)
        .arg("--update-indexed")
        .env("SEAWORLD_CRASH_AT", point)
        .env("SEAWORLD_CRASH_READY_FILE", &temp.ready_path)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("crash writer exited before {point}: {status}");
        }
        if temp.ready_path.exists() {
            child.kill().unwrap();
            let _ = child.wait().unwrap();
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash writer did not reach {point:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_old_indexed_value(path: &PathBuf) {
    let database = Database::open(path).unwrap();
    assert_eq!(
        database
            .get("users", DocumentId::new(1).unwrap())
            .unwrap()
            .unwrap()
            .get("score"),
        Some(&Value::Integer(1))
    );
    assert_index_matches_scan(&database, CompareOp::Eq, 1);
    assert_index_matches_scan(&database, CompareOp::Eq, 2);
}

#[test]
fn incomplete_index_root_publication_recovers_to_the_old_committed_state() {
    for point in [
        "transaction_before_index_page_write",
        "transaction_during_index_page_write",
        "transaction_after_index_pages_before_root",
        "transaction_before_root_publication",
        "transaction_during_root_publication",
        "transaction_after_root_before_commit",
    ] {
        let temp = TempDatabase::new();
        prepare_indexed_database(&temp.path);
        kill_index_writer(&temp, point);
        assert_old_indexed_value(&temp.path);
    }
}
