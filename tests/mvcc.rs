use std::{
    collections::BTreeMap,
    fs,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    CompareOp, Database, Document, DocumentId, Error, FieldPath, Filter, Query, QueryPlan, Value,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

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
