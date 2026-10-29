// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    Database, Document, DocumentOperation, Error, FieldPath, Filter, MAX_PROJECTION_FIELDS,
    Projection, Query, Value,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "seaworld-document-api-{}-{id}.seaworld.db",
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

#[test]
fn projections_preserve_paths_null_and_snapshot_visibility() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let id = db
        .insert(
            "items",
            Document::empty()
                .with(
                    "profile",
                    Value::object([("level".into(), Value::Integer(1))]),
                )
                .with("empty", Value::Null),
        )
        .unwrap();
    let path = FieldPath::parse("profile.level").unwrap();
    let null_path = FieldPath::parse("empty").unwrap();
    let missing = FieldPath::parse("absent").unwrap();
    let projection = Projection::new([path.clone(), null_path.clone(), missing.clone()]).unwrap();
    let query = Query::new("items", Filter::All);
    let snapshot = db.begin_read().unwrap();
    let before = db.execute_projected(&query, &projection).unwrap();
    assert_eq!(
        before,
        snapshot.execute_projected(&query, &projection).unwrap()
    );
    assert_eq!(before[0].id, id);
    assert_eq!(before[0].fields.get(&path), Some(&Value::Integer(1)));
    assert_eq!(before[0].fields.get(&null_path), Some(&Value::Null));
    assert!(!before[0].fields.contains_key(&missing));
    db.update("items", id, Document::empty()).unwrap();
    assert_eq!(
        before,
        snapshot.execute_projected(&query, &projection).unwrap()
    );
    assert!(
        db.execute_projected(&query, &projection).unwrap()[0]
            .fields
            .is_empty()
    );
    assert!(matches!(
        Projection::new(std::iter::repeat_n(path, MAX_PROJECTION_FIELDS + 1)),
        Err(Error::Limit(_))
    ));
}

#[test]
fn array_contains_rechecks_index_candidates_and_snapshot() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let first = db
        .insert(
            "items",
            Document::empty()
                .with("group", Value::Integer(1))
                .with("tags", Value::Array(vec![Value::String("a".into())])),
        )
        .unwrap();
    db.insert(
        "items",
        Document::empty().with("group", Value::Integer(1)).with(
            "tags",
            Value::Array(vec![Value::Array(vec![Value::String("a".into())])]),
        ),
    )
    .unwrap();
    db.insert("items", Document::empty().with("group", Value::Integer(1)))
        .unwrap();
    db.create_index("items", "group").unwrap();
    let contains = Filter::ArrayContains {
        path: FieldPath::parse("tags").unwrap(),
        value: Value::String("a".into()),
    };
    let indexed = Query::new(
        "items",
        Filter::And(vec![
            Filter::Compare {
                path: FieldPath::parse("group").unwrap(),
                op: seaworld_db::CompareOp::Eq,
                value: Value::Integer(1),
            },
            contains.clone(),
        ]),
    );
    let scan = Query::new("items", contains);
    let snapshot = db.begin_read().unwrap();
    assert!(matches!(
        db.explain(&indexed).unwrap(),
        seaworld_db::QueryPlan::SecondaryIndexLookup { .. }
    ));
    assert_eq!(db.execute(&indexed).unwrap().len(), 1);
    assert_eq!(db.execute(&scan).unwrap()[0].id, first);
    db.delete("items", first).unwrap();
    assert!(db.execute(&indexed).unwrap().is_empty());
    assert_eq!(snapshot.execute(&scan).unwrap()[0].id, first);
}

#[test]
fn field_updates_are_atomic_and_indexed_across_reopen() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let id = db.insert("items", Document::empty()).unwrap();
    db.create_index("items", "profile.count").unwrap();
    let count = FieldPath::parse("profile.count").unwrap();
    let name = FieldPath::parse("profile.name").unwrap();
    let mut tx = db.begin_transaction().unwrap();
    tx.update_fields(
        "items",
        id,
        &[
            DocumentOperation::Set(name.clone(), Value::String("A".into())),
            DocumentOperation::Increment(count.clone(), Value::Integer(5)),
            DocumentOperation::Decrement(count.clone(), Value::Integer(2)),
        ],
    )
    .unwrap();
    let before_error = tx.get("items", id).unwrap().unwrap().clone();
    assert!(
        tx.update_fields(
            "items",
            id,
            &[
                DocumentOperation::Unset(name.clone()),
                DocumentOperation::Increment(count.clone(), Value::Integer(i64::MAX)),
            ],
        )
        .is_err()
    );
    assert_eq!(tx.get("items", id).unwrap(), Some(&before_error));
    tx.commit().unwrap();
    let query = Query::new(
        "items",
        Filter::Compare {
            path: count.clone(),
            op: seaworld_db::CompareOp::Eq,
            value: Value::Integer(3),
        },
    );
    assert_eq!(db.execute(&query).unwrap()[0].id, id);
    let mut tx = db.begin_transaction().unwrap();
    tx.update_fields("items", id, &[DocumentOperation::Unset(name)])
        .unwrap();
    tx.rollback();
    assert_eq!(db.get("items", id).unwrap(), Some(&before_error));
    db.close().unwrap();
    let reopened = Database::open_reader(&temp.0).unwrap();
    assert_eq!(reopened.execute(&query).unwrap()[0].id, id);
    assert_eq!(reopened.get("items", id).unwrap(), Some(&before_error));
}

#[test]
fn public_api_metadata_and_error_codes_are_transport_neutral() {
    assert_eq!(seaworld_db::API_VERSION, "0.1");
    assert!(seaworld_db::capabilities().projections);
    assert!(!seaworld_db::capabilities().durable_request_deduplication);
    let request_id = seaworld_db::RequestId::new("operation-17").unwrap();
    assert_eq!(request_id.as_str(), "operation-17");
    assert!(seaworld_db::RequestId::new("").is_err());
    assert!(seaworld_db::RequestId::new("space in id").is_err());
    assert!(seaworld_db::RequestId::new("x".repeat(129)).is_err());
    assert_eq!(Error::Locked.code(), seaworld_db::ErrorCode::Locked);
    assert_eq!(
        Error::Integrity("detail".into()).code(),
        seaworld_db::ErrorCode::Corrupt
    );
}

#[test]
fn bulk_updates_and_deletes_are_atomic_and_snapshot_safe() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let first = db
        .insert("items", Document::empty().with("n", Value::Integer(1)))
        .unwrap();
    let second = db
        .insert(
            "items",
            Document::empty().with("n", Value::String("wrong".into())),
        )
        .unwrap();
    let query = Query::new("items", Filter::All);
    let path = FieldPath::parse("n").unwrap();
    let snapshot = db.begin_read().unwrap();
    let increment = [DocumentOperation::Increment(
        path.clone(),
        Value::Integer(1),
    )];
    assert!(db.update_many(&query, &increment).is_err());
    assert_eq!(
        db.get("items", first).unwrap().unwrap().get("n"),
        Some(&Value::Integer(1))
    );
    assert_eq!(db.update_one(&query, &increment).unwrap(), 1);
    assert_eq!(
        db.get("items", first).unwrap().unwrap().get("n"),
        Some(&Value::Integer(2))
    );
    assert_eq!(db.delete_one(&query).unwrap(), 1);
    assert_eq!(db.delete_many(&query).unwrap(), 1);
    assert!(db.get("items", second).unwrap().is_none());
    assert_eq!(snapshot.execute(&query).unwrap().len(), 2);
    drop(snapshot);
    db.close().unwrap();
    let reopened = Database::open_reader(&temp.0).unwrap();
    assert!(reopened.execute(&query).unwrap().is_empty());
}

#[test]
fn public_engine_boundary_uses_the_existing_backend() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    let engine: &mut dyn seaworld_db::PublicEngine = &mut database;
    engine.create_collection("items").unwrap();
    let mut tx = engine.begin().unwrap();
    let id = tx.insert("items", Document::empty().with("name", Value::String("A".into()))).unwrap();
    assert!(tx.get("items", id).unwrap().is_some());
    tx.commit().unwrap();
    assert_eq!(engine.query(&Query::new("items", Filter::All)).unwrap().len(), 1);
    assert!(engine.get("items", id).unwrap().is_some());
    assert_eq!(engine.delete(&Query::new("items", Filter::IdEq(id))).unwrap(), 1);
}
