// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    collections::BTreeMap,
    fs,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use seaworld_db::{
    CompareOp, Database, Document, DocumentId, Error, FieldPath, Filter, Query, QueryPlan,
    SortDirection, Value,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);
const DIFFERENTIAL_SEED: u64 = 0xd1ff_e2e5;

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "seaworld-mvcc-{}-{id}.seaworld.db",
            std::process::id()
        )))
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(format!("{}.writer.lock", self.0.display()));
    }
}

fn document(value: i64) -> Document {
    Document::empty().with("value", Value::Integer(value))
}

fn scalar_document(value: Option<Value>) -> Document {
    value.map_or_else(Document::empty, |value| {
        Document::empty().with("value", value)
    })
}

fn next_seed(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn generated_value(seed: u64) -> Option<Value> {
    match seed % 8 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(Value::Bool(seed & 1 == 0)),
        3 => Some(Value::Integer((seed % 9) as i64 - 4)),
        4 => Some(Value::Float((seed % 7) as f64 / 2.0 - 1.5)),
        5 => Some(Value::String(format!("group-{}", seed % 3))),
        6 => Some(Value::Integer((seed % 5) as i64 - 2)),
        _ => Some(Value::String(format!("group-{}", seed % 2))),
    }
}

fn normalized_rows(database: &Database, query: &Query) -> Vec<(i64, Document)> {
    let mut rows: Vec<_> = database
        .execute(query)
        .unwrap()
        .into_iter()
        .map(|row| (row.id.value(), (*row.document).clone()))
        .collect();
    rows.sort_unstable_by_key(|(id, _)| *id);
    rows
}

fn assert_index_matches_full_scan(indexed: &Database, full_scan: &Database, seed: u64) {
    let path = FieldPath::parse("value").unwrap();
    let values = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Integer(-2),
        Value::Integer(0),
        Value::Integer(3),
        Value::Float(-1.0),
        Value::Float(0.0),
        Value::Float(1.5),
        Value::String("group-0".into()),
        Value::String("group-1".into()),
        Value::String("group-2".into()),
    ];
    let operations = [
        CompareOp::Eq,
        CompareOp::Ne,
        CompareOp::Lt,
        CompareOp::Lte,
        CompareOp::Gt,
        CompareOp::Gte,
    ];

    for value in values {
        for op in operations {
            let query = Query::new(
                "items",
                Filter::Compare {
                    path: path.clone(),
                    op,
                    value: value.clone(),
                },
            );
            assert_eq!(
                normalized_rows(indexed, &query),
                normalized_rows(full_scan, &query),
                "initial seed {DIFFERENTIAL_SEED:#x}, generator state {seed:#x}, operation {op:?}, value {value:?}"
            );
        }
    }

    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        let query = Query::new(
            "items",
            Filter::Exists {
                path: path.clone(),
                exists: true,
            },
        )
        .sort_by(path.clone(), direction)
        .limit(7)
        .offset(2);
        let indexed_rows: Vec<_> = indexed
            .execute(&query)
            .unwrap()
            .into_iter()
            .map(|row| (row.id.value(), (*row.document).clone()))
            .collect();
        let full_scan_rows: Vec<_> = full_scan
            .execute(&query)
            .unwrap()
            .into_iter()
            .map(|row| (row.id.value(), (*row.document).clone()))
            .collect();
        assert_eq!(
            indexed_rows, full_scan_rows,
            "initial seed {DIFFERENTIAL_SEED:#x}, generator state {seed:#x}"
        );
    }
}

#[test]
fn mvcc_snapshots_keep_a_stable_document_view_across_update_and_delete() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", document(10)).unwrap();
    let before = database.begin_read().unwrap();

    database.update("users", id, document(20)).unwrap();
    let after_update = database.begin_read().unwrap();
    database.delete("users", id).unwrap();

    assert_eq!(before.get("users", id).unwrap(), Some(document(10)));
    assert_eq!(after_update.get("users", id).unwrap(), Some(document(20)));
    assert_eq!(
        database.begin_read().unwrap().get("users", id).unwrap(),
        None
    );
    assert!(before.version() < after_update.version());
}

#[test]
fn fixed_readers_coexist_with_one_writer_and_reject_mutations() {
    let temp = TempDatabase::new();
    let mut writer = Database::create(&temp.0).unwrap();
    writer.create_collection("users").unwrap();
    let id = writer.insert("users", document(1)).unwrap();
    let mut reader = Database::open_reader(&temp.0).unwrap();

    writer.update("users", id, document(2)).unwrap();
    assert_eq!(reader.get("users", id).unwrap(), Some(&document(1)));
    assert!(matches!(
        reader.insert("users", document(3)),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(Database::open(&temp.0), Err(Error::Locked)));
}

#[test]
fn two_read_only_handles_coexist_with_the_writer() {
    let temp = TempDatabase::new();
    let mut writer = Database::create(&temp.0).unwrap();
    writer.create_collection("users").unwrap();
    let id = writer.insert("users", document(1)).unwrap();
    let reader_a = Database::open_reader(&temp.0).unwrap();
    let reader_b = Database::open_reader(&temp.0).unwrap();

    writer.update("users", id, document(2)).unwrap();

    assert_eq!(reader_a.get("users", id).unwrap(), Some(&document(1)));
    assert_eq!(reader_b.get("users", id).unwrap(), Some(&document(1)));
    assert_eq!(writer.get("users", id).unwrap(), Some(&document(2)));
}

#[test]
fn snapshot_clones_read_consistently_from_threads_during_commits() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", document(1)).unwrap();
    let snapshot = database.begin_read().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut readers = Vec::new();

    for _ in 0..2 {
        let snapshot = snapshot.clone();
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            let mut observed = Vec::new();
            for _ in 2..=17 {
                barrier.wait();
                observed.push(snapshot.get("users", id).unwrap());
                barrier.wait();
            }
            observed
        }));
    }

    for value in 2..=17 {
        barrier.wait();
        database.update("users", id, document(value)).unwrap();
        barrier.wait();
    }

    for reader in readers {
        assert_eq!(reader.join().unwrap(), vec![Some(document(1)); 16]);
    }
    assert_eq!(database.get("users", id).unwrap(), Some(&document(17)));
}

#[test]
fn rollback_does_not_advance_or_publish_a_snapshot_version() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", document(1)).unwrap();
    let before = database.begin_read().unwrap();
    let version = before.version();

    let mut transaction = database.begin_transaction().unwrap();
    assert!(transaction.update("users", id, document(2)).unwrap());
    transaction.rollback();

    let after_rollback = database.begin_read().unwrap();
    assert_eq!(after_rollback.version(), version);
    assert_eq!(after_rollback.get("users", id).unwrap(), Some(document(1)));

    let mut transaction = database.begin_transaction().unwrap();
    assert!(transaction.update("users", id, document(3)).unwrap());
    transaction.commit().unwrap();
    assert_eq!(before.get("users", id).unwrap(), Some(document(1)));
    assert_eq!(
        database.begin_read().unwrap().get("users", id).unwrap(),
        Some(document(3))
    );
}

#[test]
fn read_only_open_reports_a_torn_tail_without_changing_the_file() {
    let temp = TempDatabase::new();
    Database::create(&temp.0).unwrap().close().unwrap();
    OpenOptions::new()
        .append(true)
        .open(&temp.0)
        .unwrap()
        .write_all(b"SWB")
        .unwrap();
    let before = fs::read(&temp.0).unwrap();

    assert!(matches!(
        Database::open_reader(&temp.0),
        Err(Error::RecoveryRequired)
    ));
    assert_eq!(fs::read(&temp.0).unwrap(), before);

    let writer = Database::open(&temp.0).unwrap();
    assert_eq!(writer.recovery_report().discarded_bytes(), 3);
}

#[test]
fn indexed_numeric_range_matches_the_filter_and_survives_reopen() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    for value in [3, 9, 10, 11, 17, 21] {
        database.insert("users", document(value)).unwrap();
    }
    database.create_index("users", "value").unwrap();
    let query = Query::new(
        "users",
        Filter::Compare {
            path: FieldPath::parse("value").unwrap(),
            op: CompareOp::Gte,
            value: Value::Integer(10),
        },
    );

    assert!(matches!(
        database.explain(&query).unwrap(),
        QueryPlan::SecondaryIndexRangeLookup { .. }
    ));
    assert_eq!(
        database
            .execute(&query)
            .unwrap()
            .iter()
            .map(|row| row.document.get("value").unwrap().clone())
            .collect::<Vec<_>>(),
        [10, 11, 17, 21]
            .into_iter()
            .map(Value::Integer)
            .collect::<Vec<_>>()
    );
    database.close().unwrap();

    let database = Database::open(&temp.0).unwrap();
    assert!(matches!(
        database.explain(&query).unwrap(),
        QueryPlan::SecondaryIndexRangeLookup { .. }
    ));
    assert_eq!(database.execute(&query).unwrap().len(), 4);
}

#[test]
fn snapshots_and_compaction_preserve_version_and_active_history_rules() {
    let source = TempDatabase::new();
    let target = TempDatabase::new();
    let mut database = Database::create(&source.0).unwrap();
    database.create_collection("users").unwrap();
    let id = database.insert("users", document(1)).unwrap();
    database.update("users", id, document(2)).unwrap();
    let version = database.current_version().unwrap();
    let snapshot = database.begin_read().unwrap();

    assert!(matches!(
        database.write_compacted_copy(&target.0),
        Err(Error::ActiveSnapshots)
    ));
    drop(snapshot);
    database.write_compacted_copy(&target.0).unwrap();

    let compacted = Database::open(&target.0).unwrap();
    assert_eq!(compacted.get("users", id).unwrap(), Some(&document(2)));
    assert!(compacted.current_version().unwrap() >= version);
}

#[test]
fn deterministic_state_machine_matches_retained_snapshot_models() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    let mut model = BTreeMap::<DocumentId, i64>::new();
    let mut snapshots = Vec::new();
    let mut seed = 0x5eed_u64;

    for step in 0..96_i64 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let raw_id = (seed % 12 + 1) as i64;
        let id = DocumentId::new(raw_id).unwrap();
        match (seed >> 8) % 3 {
            0 if !model.contains_key(&id) => {
                let value = step * 3;
                database
                    .insert_with_id("users", id, document(value))
                    .unwrap();
                model.insert(id, value);
            }
            1 if model.contains_key(&id) => {
                let value = step * 5;
                database.update("users", id, document(value)).unwrap();
                model.insert(id, value);
            }
            2 if model.remove(&id).is_some() => {
                assert!(database.delete("users", id).unwrap());
            }
            _ => {}
        }

        if step % 8 == 0 {
            snapshots.push((database.begin_read().unwrap(), model.clone()));
        }
    }

    for (snapshot, expected) in snapshots {
        let expected_ids: Vec<_> = expected.keys().copied().collect();
        assert_eq!(snapshot.document_ids("users").unwrap(), expected_ids);
        for (id, value) in expected {
            assert_eq!(snapshot.get("users", id).unwrap(), Some(document(value)));
        }
    }
}

#[test]
fn seeded_mutations_keep_indexed_queries_equal_to_full_scan_before_and_after_reopen() {
    let indexed_file = TempDatabase::new();
    let full_scan_file = TempDatabase::new();
    let mut indexed = Database::create(&indexed_file.0).unwrap();
    let mut full_scan = Database::create(&full_scan_file.0).unwrap();
    indexed.create_collection("items").unwrap();
    full_scan.create_collection("items").unwrap();
    indexed.create_index("items", "value").unwrap();
    let mut present = BTreeMap::<i64, Option<Value>>::new();
    let mut seed = DIFFERENTIAL_SEED;

    for step in 0..128_i64 {
        let random = next_seed(&mut seed);
        let id = DocumentId::new((random % 24 + 1) as i64).unwrap();
        match (random >> 8) % 3 {
            0 if !present.contains_key(&id.value()) => {
                let value = generated_value(random >> 16);
                let document = scalar_document(value.clone());
                indexed
                    .insert_with_id("items", id, document.clone())
                    .unwrap();
                full_scan.insert_with_id("items", id, document).unwrap();
                present.insert(id.value(), value);
            }
            1 if present.contains_key(&id.value()) => {
                let value = generated_value(random >> 16);
                let document = scalar_document(value.clone());
                assert!(indexed.update("items", id, document.clone()).unwrap());
                assert!(full_scan.update("items", id, document).unwrap());
                present.insert(id.value(), value);
            }
            2 if present.remove(&id.value()).is_some() => {
                assert!(indexed.delete("items", id).unwrap());
                assert!(full_scan.delete("items", id).unwrap());
            }
            _ => {}
        }

        if step % 32 == 31 {
            assert_index_matches_full_scan(&indexed, &full_scan, seed);
        }
    }

    assert_index_matches_full_scan(&indexed, &full_scan, seed);
    assert!(matches!(
        indexed
            .explain(&Query::new(
                "items",
                Filter::Compare {
                    path: FieldPath::parse("value").unwrap(),
                    op: CompareOp::Gte,
                    value: Value::Integer(0),
                },
            ))
            .unwrap(),
        QueryPlan::SecondaryIndexRangeLookup { .. }
    ));

    indexed.close().unwrap();
    full_scan.close().unwrap();
    let indexed = Database::open(&indexed_file.0).unwrap();
    let full_scan = Database::open(&full_scan_file.0).unwrap();
    assert_index_matches_full_scan(&indexed, &full_scan, seed);
}

#[test]
fn deterministic_malformed_record_tails_do_not_panic_during_open() {
    const INITIAL_SEED: u64 = 0xc0ff_eec7_u64;
    let mut seed = INITIAL_SEED;

    for case in 0..64 {
        let temp = TempDatabase::new();
        Database::create(&temp.0).unwrap().close().unwrap();
        let mut bytes = fs::read(&temp.0).unwrap();
        let tail_length = (next_seed(&mut seed) % 128 + 1) as usize;
        for _ in 0..tail_length {
            bytes.push(next_seed(&mut seed) as u8);
        }
        fs::write(&temp.0, bytes).unwrap();

        let result = std::panic::catch_unwind(|| Database::open(&temp.0));
        assert!(
            result.is_ok(),
            "open panicked for malformed tail case {case}, initial seed {INITIAL_SEED:#x}, state {seed:#x}"
        );
        if let Ok(database) = result.unwrap() {
            database.close().unwrap();
        }
    }
}
