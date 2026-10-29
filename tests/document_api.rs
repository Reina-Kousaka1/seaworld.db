// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    Database, Document, DocumentOperation, EngineHealth, Error, ErrorCode, FieldPath, Filter,
    MAX_PROJECTION_FIELDS, OperationContext, Projection, PublicRowValue, Query, Value,
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
    assert_eq!(Error::Locked.code().as_str(), "locked");
    assert_eq!(
        seaworld_db::capabilities().supports("projections"),
        Some(true)
    );
    assert_eq!(seaworld_db::capabilities().supports("unknown"), None);
    assert_eq!(
        Error::Integrity("detail".into()).code(),
        ErrorCode::IntegrityFailure
    );
    assert_eq!(
        Error::UnsupportedFormatVersion(99).code(),
        ErrorCode::UnknownFormatVersion
    );
    assert_eq!(
        Error::UnsupportedFeature("flag".into()).code(),
        ErrorCode::IncompatibleFormat
    );
    let context = OperationContext::new().with_request_id(request_id);
    assert_eq!(context.request_id().unwrap().as_str(), "operation-17");
}

#[test]
fn unique_conflict_in_bulk_update_does_not_publish_any_change() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let first = db
        .insert("items", Document::empty().with("key", Value::Integer(1)))
        .unwrap();
    let second = db
        .insert("items", Document::empty().with("key", Value::Integer(2)))
        .unwrap();
    db.create_unique_index("items", "key").unwrap();
    let query = Query::new("items", Filter::All);
    let op = DocumentOperation::Set(FieldPath::parse("key").unwrap(), Value::Integer(3));
    assert!(matches!(
        db.update_many(&query, &[op]),
        Err(Error::ConstraintViolation(_))
    ));
    assert_eq!(
        db.get("items", first).unwrap().unwrap().get("key"),
        Some(&Value::Integer(1))
    );
    assert_eq!(
        db.get("items", second).unwrap().unwrap().get("key"),
        Some(&Value::Integer(2))
    );
    db.close().unwrap();
    let reopened = Database::open_reader(&temp.0).unwrap();
    assert_eq!(
        reopened.get("items", first).unwrap().unwrap().get("key"),
        Some(&Value::Integer(1))
    );
    assert_eq!(
        reopened.get("items", second).unwrap().unwrap().get("key"),
        Some(&Value::Integer(2))
    );
}

#[test]
fn oversized_intermediate_update_is_rejected_without_changing_input() {
    let original = Document::empty().with("first", Value::Binary(vec![0; 9 * 1024 * 1024]));
    let operations = [
        DocumentOperation::Set(
            FieldPath::parse("second").unwrap(),
            Value::Binary(vec![0; 9 * 1024 * 1024]),
        ),
        DocumentOperation::Unset(FieldPath::parse("first").unwrap()),
    ];
    assert!(matches!(
        seaworld_db::apply_document_operations(&original, &operations),
        Err(Error::Limit(_))
    ));
    assert!(original.get("second").is_none());
    assert!(original.get("first").is_some());
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
    assert_eq!(engine.api_info().version, seaworld_db::API_VERSION);
    assert_eq!(engine.health(), EngineHealth::Ready);
    engine.create_collection("items").unwrap();
    let mut tx = engine.begin().unwrap();
    let id = tx
        .insert(
            "items",
            Document::empty().with("name", Value::String("A".into())),
        )
        .unwrap();
    assert!(tx.get("items", id).unwrap().is_some());
    tx.commit().unwrap();
    assert_eq!(
        engine
            .query(&Query::new("items", Filter::All))
            .unwrap()
            .len(),
        1
    );
    assert!(engine.get("items", id).unwrap().is_some());
    let projection = Projection::new([
        FieldPath::parse("name").unwrap(),
        FieldPath::parse("missing").unwrap(),
    ])
    .unwrap();
    let projected = engine
        .query_projected(&Query::new("items", Filter::All), &projection)
        .unwrap();
    assert_eq!(projected[0].id, id);
    assert_eq!(projected[0].document_value(), None);
    let fields = projected[0].projected_fields().unwrap();
    assert_eq!(
        fields.get(&FieldPath::parse("name").unwrap()),
        Some(&Value::String("A".into()))
    );
    assert!(!fields.contains_key(&FieldPath::parse("missing").unwrap()));
    assert!(matches!(&projected[0].value, PublicRowValue::Projection(_)));
    let rows = engine.query(&Query::new("items", Filter::All)).unwrap();
    assert!(matches!(&rows[0].value, PublicRowValue::Document(_)));
    assert_eq!(engine.collection_names().unwrap(), ["items"]);
    assert_eq!(
        engine
            .delete(&Query::new("items", Filter::IdEq(id)))
            .unwrap(),
        1
    );
}

#[test]
fn upsert_uses_explicit_insert_base_and_existing_match() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("items").unwrap();
    let path = FieldPath::parse("key").unwrap();
    let query = Query::new(
        "items",
        Filter::Compare {
            path,
            op: seaworld_db::CompareOp::Eq,
            value: Value::String("x".into()),
        },
    );
    let operations = [DocumentOperation::Increment(
        FieldPath::parse("n").unwrap(),
        Value::Integer(1),
    )];
    let base = Document::empty().with("key", Value::String("x".into()));
    let (id, inserted) = db.upsert_one(&query, base.clone(), &operations).unwrap();
    assert!(inserted);
    assert_eq!(
        db.upsert_one(&query, base, &operations).unwrap(),
        (id, false)
    );
    assert_eq!(
        db.get("items", id).unwrap().unwrap().get("n"),
        Some(&Value::Integer(2))
    );
    assert!(
        db.upsert_one(&query.clone().limit(0), Document::empty(), &operations)
            .is_err()
    );
    db.close().unwrap();
    assert_eq!(
        Database::open_reader(&temp.0)
            .unwrap()
            .get("items", id)
            .unwrap()
            .unwrap()
            .get("n"),
        Some(&Value::Integer(2))
    );
}

#[test]
fn document_operations_survive_backup_restore_and_compaction() {
    let source = TempDatabase::new();
    let backup = TempDatabase::new();
    let restored = TempDatabase::new();
    let compacted = TempDatabase::new();
    let mut db = Database::create(&source.0).unwrap();
    db.create_collection("items").unwrap();
    db.create_index("items", "count").unwrap();
    let id = db
        .insert(
            "items",
            Document::empty().with("tags", Value::Array(vec![Value::String("blue".into())])),
        )
        .unwrap();
    db.update_one(
        &Query::new("items", Filter::IdEq(id)),
        &[DocumentOperation::Increment(
            FieldPath::parse("count").unwrap(),
            Value::Integer(3),
        )],
    )
    .unwrap();
    let query = Query::new(
        "items",
        Filter::And(vec![
            Filter::Compare {
                path: FieldPath::parse("count").unwrap(),
                op: seaworld_db::CompareOp::Eq,
                value: Value::Integer(3),
            },
            Filter::ArrayContains {
                path: FieldPath::parse("tags").unwrap(),
                value: Value::String("blue".into()),
            },
        ]),
    );
    assert_eq!(db.execute(&query).unwrap()[0].id, id);
    db.backup(&backup.0).unwrap();
    Database::restore_backup(&backup.0, &restored.0).unwrap();
    let restored_db = Database::open_reader(&restored.0).unwrap();
    assert_eq!(restored_db.execute(&query).unwrap()[0].id, id);
    restored_db.write_v4_compacted_copy(&compacted.0).unwrap();
    let compacted_db = Database::open_reader(&compacted.0).unwrap();
    assert_eq!(compacted_db.execute(&query).unwrap()[0].id, id);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 16, ..proptest::test_runner::Config::default() })]
    #[test]
    fn generated_updates_match_integer_model(steps in proptest::collection::vec((0u8..4, -100i64..100), 0..64)) {
        let path = FieldPath::parse("counter").unwrap();
        let mut document = Document::empty();
        let mut model: Option<i64> = None;
        for (kind, value) in steps {
            let op = match kind {
                0 => { model = Some(value); DocumentOperation::Set(path.clone(), Value::Integer(value)) }
                1 => { model = None; DocumentOperation::Unset(path.clone()) }
                2 => { model = Some(model.unwrap_or(0) + value); DocumentOperation::Increment(path.clone(), Value::Integer(value)) }
                _ => { model = Some(model.unwrap_or(0) - value); DocumentOperation::Decrement(path.clone(), Value::Integer(value)) }
            };
            document = seaworld_db::apply_document_operations(&document, &[op]).unwrap();
            let expected = model.map(Value::Integer);
            proptest::prop_assert_eq!(document.get("counter"), expected.as_ref());
        }
    }

    #[test]
    fn generated_indexed_array_predicates_match_full_scans(values in proptest::collection::vec((0i64..5, proptest::prelude::any::<bool>()), 1..9), target in 0i64..5) {
        let temp = TempDatabase::new();
        let mut db = Database::create(&temp.0).unwrap();
        db.create_collection("items").unwrap();
        for (group, contains) in values {
            let tags = if contains { vec![Value::String("a".into())] } else { vec![] };
            db.insert("items", Document::empty().with("group", Value::Integer(group)).with("tags", Value::Array(tags))).unwrap();
        }
        db.create_index("items", "group").unwrap();
        let predicate = Filter::And(vec![
            Filter::Compare { path: FieldPath::parse("group").unwrap(), op: seaworld_db::CompareOp::Eq, value: Value::Integer(target) },
            Filter::ArrayContains { path: FieldPath::parse("tags").unwrap(), value: Value::String("a".into()) },
        ]);
        let indexed = Query::new("items", predicate.clone());
        let scan = Query::new("items", Filter::Not(Box::new(Filter::Not(Box::new(predicate)))));
        let rows = |db: &Database, q: &Query| db.execute(q).unwrap().into_iter().map(|row| row.id).collect::<Vec<_>>();
        proptest::prop_assert_eq!(rows(&db, &indexed), rows(&db, &scan));
        let snapshot = db.begin_read().unwrap();
        let before = snapshot.execute(&scan).unwrap();
        db.delete_many(&indexed).unwrap();
        proptest::prop_assert_eq!(snapshot.execute(&scan).unwrap(), before);
        proptest::prop_assert_eq!(rows(&db, &indexed), rows(&db, &scan));
        drop(snapshot);
        db.close().unwrap();
        let db = Database::open_reader(&temp.0).unwrap();
        proptest::prop_assert_eq!(rows(&db, &indexed), rows(&db, &scan));
    }

    #[test]
    fn generated_document_bulk_upserts_match_a_small_reference_model(
        steps in proptest::collection::vec((0u8..7, 0i64..4, -8i64..9), 0..24)
    ) {
        let temp = TempDatabase::new();
        let mut db = Database::create(&temp.0).unwrap();
        db.create_collection("items").unwrap();
        let make_document = |group: i64, value: i64, tagged: bool| {
            Document::empty()
                .with("group", Value::Integer(group))
                .with("n", Value::Integer(value))
                .with("tags", Value::Array(if tagged { vec![Value::String("a".into())] } else { Vec::new() }))
        };
        let mut model = BTreeMap::new();
        for (group, value, tagged) in [(0, 0, true), (1, 1, false), (2, 2, true)] {
            let id = db.insert("items", make_document(group, value, tagged)).unwrap();
            model.insert(id.value(), make_document(group, value, tagged));
        }
        db.create_index("items", "group").unwrap();
        let snapshot = db.begin_read().unwrap();
        let initial = snapshot.execute(&Query::new("items", Filter::All)).unwrap();
        let group_path = FieldPath::parse("group").unwrap();
        let n_path = FieldPath::parse("n").unwrap();
        let tags_path = FieldPath::parse("tags").unwrap();
        let rows = |database: &Database| {
            database.execute(&Query::new("items", Filter::All)).unwrap().into_iter()
                .map(|row| (row.id.value(), (*row.document).clone())).collect::<Vec<_>>()
        };
        let model_rows = |state: &BTreeMap<i64, Document>| {
            state.iter().map(|(id, document)| (*id, document.clone())).collect::<Vec<_>>()
        };
        let matches_group = |document: &Document, group: i64, tagged_only: bool| {
            document.get("group") == Some(&Value::Integer(group))
                && (!tagged_only || matches!(document.get("tags"), Some(Value::Array(tags)) if tags.contains(&Value::String("a".into()))))
        };

        for (kind, group, value) in steps {
            let indexed_filter = Filter::And(vec![
                Filter::Compare { path: group_path.clone(), op: seaworld_db::CompareOp::Eq, value: Value::Integer(group) },
                Filter::ArrayContains { path: tags_path.clone(), value: Value::String("a".into()) },
            ]);
            let indexed = Query::new("items", indexed_filter.clone());
            let full_scan = Query::new("items", Filter::Not(Box::new(Filter::Not(Box::new(indexed_filter)))));
            let indexed_ids = db.execute(&indexed).unwrap().into_iter().map(|row| row.id).collect::<Vec<_>>();
            let scan_ids = db.execute(&full_scan).unwrap().into_iter().map(|row| row.id).collect::<Vec<_>>();
            proptest::prop_assert_eq!(indexed_ids, scan_ids);

            match kind {
                0 => {
                    let operation = DocumentOperation::Set(n_path.clone(), Value::Integer(value));
                    let expected_ids = model.iter().filter_map(|(id, document)| matches_group(document, group, true).then_some(*id)).collect::<Vec<_>>();
                    proptest::prop_assert_eq!(db.update_many(&indexed, &[operation]).unwrap(), expected_ids.len());
                    for id in expected_ids { let document = model.remove(&id).unwrap().with("n", Value::Integer(value)); model.insert(id, document); }
                }
                1 => {
                    let query = Query::new("items", Filter::Compare { path: group_path.clone(), op: seaworld_db::CompareOp::Eq, value: Value::Integer(group) });
                    let expected_ids = model.iter().filter_map(|(id, document)| matches_group(document, group, false).then_some(*id)).collect::<Vec<_>>();
                    proptest::prop_assert_eq!(db.delete_many(&query).unwrap(), expected_ids.len());
                    for id in expected_ids { model.remove(&id); }
                }
                2 => {
                    let query = Query::new("items", Filter::Compare { path: group_path.clone(), op: seaworld_db::CompareOp::Eq, value: Value::Integer(group) });
                    let existing = model.iter().find_map(|(id, document)| matches_group(document, group, false).then_some(*id));
                    let base = make_document(group, 0, true);
                    let (id, inserted) = db.upsert_one(&query, base.clone(), &[DocumentOperation::Set(n_path.clone(), Value::Integer(value))]).unwrap();
                    proptest::prop_assert_eq!(inserted, existing.is_none());
                    proptest::prop_assert_eq!(id.value(), existing.unwrap_or(id.value()));
                    let document = if inserted { base.with("n", Value::Integer(value)) } else { model.remove(&id.value()).unwrap().with("n", Value::Integer(value)) };
                    model.insert(id.value(), document);
                }
                3 => {
                    let query = Query::new("items", Filter::Compare { path: group_path.clone(), op: seaworld_db::CompareOp::Eq, value: Value::Integer(group) });
                    let expected_ids = model.iter().filter_map(|(id, document)| matches_group(document, group, false).then_some(*id)).collect::<Vec<_>>();
                    let operation = DocumentOperation::Increment(n_path.clone(), Value::Integer(value));
                    proptest::prop_assert_eq!(db.update_many(&query, &[operation]).unwrap(), expected_ids.len());
                    for id in expected_ids {
                        let n = match model.get(&id).unwrap().get("n") {
                            Some(Value::Integer(n)) => *n,
                            None => 0,
                            _ => unreachable!(),
                        };
                        let document = model.remove(&id).unwrap().with("n", Value::Integer(n + value));
                        model.insert(id, document);
                    }
                }
                4 => if let Some(id) = model.keys().next().copied() {
                    let query = Query::new("items", Filter::IdEq(seaworld_db::DocumentId::new(id).unwrap()));
                    proptest::prop_assert_eq!(db.update_one(&query, &[DocumentOperation::Unset(n_path.clone())]).unwrap(), 1);
                    let document = model.remove(&id).unwrap();
                    let mut fields = document.fields().clone();
                    fields.remove("n");
                    model.insert(id, Document::new(fields));
                },
                5 => if let Some(id) = model.keys().next().copied() {
                    let query = Query::new("items", Filter::IdEq(seaworld_db::DocumentId::new(id).unwrap()));
                    proptest::prop_assert_eq!(db.delete_one(&query).unwrap(), 1);
                    model.remove(&id);
                },
                _ => {
                    let values = vec![Value::Integer(group), Value::Integer((group + 1) % 4)];
                    let query = Query::new("items", Filter::in_values(group_path.clone(), values).unwrap());
                    let expected_ids = model.iter().filter_map(|(id, document)| {
                        matches_group(document, group, false).then_some(*id).or_else(|| matches_group(document, (group + 1) % 4, false).then_some(*id))
                    }).collect::<Vec<_>>();
                    proptest::prop_assert_eq!(db.update_many(&query, &[DocumentOperation::Set(n_path.clone(), Value::Integer(value))]).unwrap(), expected_ids.len());
                    for id in expected_ids { let document = model.remove(&id).unwrap().with("n", Value::Integer(value)); model.insert(id, document); }
                }
            }
            proptest::prop_assert_eq!(rows(&db), model_rows(&model));
            proptest::prop_assert_eq!(
                snapshot.execute(&Query::new("items", Filter::All)).unwrap(),
                initial.clone()
            );
        }
        drop(snapshot);
        db.close().unwrap();
        let reopened = Database::open_reader(&temp.0).unwrap();
        proptest::prop_assert_eq!(rows(&reopened), model_rows(&model));
    }
}
