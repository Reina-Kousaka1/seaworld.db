// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    CompareOp, Database, Document, Error, FieldPath, Query, QueryPlan, SqlResult, Value,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "seaworld-sql-{}-{id}.seaworld.db",
            std::process::id()
        )))
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn sql_document_queries_share_native_planner_and_persist_mutations() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    assert!(matches!(
        database
            .execute_sql("CREATE COLLECTION users", &[])
            .unwrap(),
        SqlResult::CollectionCreated(_)
    ));
    assert_eq!(
        database
            .execute_sql(
                "INSERT INTO users VALUES {\"username\": \"Charlotte\", \"credits\": 1250, \"profile\": {\"level\": 17, \"career\": \"ballet\"}, \"inventory\": [\"pink_leotard\", \"ballet_shoes\"]}",
                &[],
            )
            .unwrap(),
        SqlResult::AffectedRows(1)
    );
    database.create_index("users", "credits").unwrap();

    let plan = database
        .execute_sql(
            "EXPLAIN SELECT username FROM users WHERE credits >= ?",
            &[Value::Integer(1_000)],
        )
        .unwrap();
    assert!(matches!(
        plan,
        SqlResult::Plan(QueryPlan::SecondaryIndexRangeLookup { .. })
    ));
    let selected = database
        .execute_sql(
            "SELECT username, profile.level FROM users WHERE profile.career = 'ballet'",
            &[],
        )
        .unwrap();
    let SqlResult::Rows(rows) = selected else {
        panic!("SELECT must return rows")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values.get("username"),
        Some(&Value::String("Charlotte".into()))
    );
    assert_eq!(
        rows[0].values.get("profile.level"),
        Some(&Value::Integer(17))
    );

    assert_eq!(
        database
            .execute_sql(
                "UPDATE users SET credits = ?, profile.level = 19 WHERE username = ?",
                &[Value::Integer(1_500), Value::String("Charlotte".into())],
            )
            .unwrap(),
        SqlResult::AffectedRows(1)
    );
    assert_eq!(
        database
            .execute_sql("DELETE FROM users WHERE credits < ?", &[Value::Integer(10)])
            .unwrap(),
        SqlResult::AffectedRows(0)
    );
    database.close().unwrap();

    let mut database = Database::open(&temp.0).unwrap();
    let native = Query::new(
        "users",
        seaworld_db::Filter::Compare {
            path: FieldPath::parse("credits").unwrap(),
            op: CompareOp::Eq,
            value: Value::Integer(1_500),
        },
    );
    assert_eq!(database.execute(&native).unwrap().len(), 1);
    assert_eq!(
        database
            .execute_sql("SELECT profile.level FROM users WHERE credits = 1500", &[])
            .unwrap(),
        SqlResult::Rows(vec![seaworld_db::SqlRow {
            id: rows_id(&database),
            values: Document::empty().with("profile.level", Value::Integer(19)),
        }])
    );
}

fn rows_id(database: &Database) -> seaworld_db::DocumentId {
    database.document_ids("users").unwrap()[0]
}

#[test]
fn sql_parser_reports_locations_and_rejects_unbound_or_extra_parameters() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("users").unwrap();
    assert!(matches!(
        database.execute_sql("SELECT * FROM users WHERE score = ?", &[]),
        Err(Error::Query(message)) if message.contains("byte")
    ));
    assert!(matches!(
        database.execute_sql("SELECT * FROM users WHERE score = 1", &[Value::Integer(1)]),
        Err(Error::Query(message)) if message.contains("parameter count")
    ));
    assert!(matches!(
        database.execute_sql("SELECT * FROM users; SELECT * FROM users", &[]),
        Err(Error::Query(message)) if message.contains("byte")
    ));
    assert!(matches!(
        database.execute_sql("INSERT INTO users VALUES {\"a\": 1, \"a\": 2}", &[]),
        Err(Error::Query(message)) if message.contains("duplicate")
    ));
}

#[test]
fn sql_distinguishes_explicit_null_from_missing_fields() {
    let temp = TempDatabase::new();
    let mut database = Database::create(&temp.0).unwrap();
    database.create_collection("values").unwrap();
    database
        .insert("values", Document::empty().with("value", Value::Null))
        .unwrap();
    database.insert("values", Document::empty()).unwrap();
    database
        .insert("values", Document::empty().with("value", Value::Integer(1)))
        .unwrap();
    database.create_index("values", "value").unwrap();

    let SqlResult::Rows(null_rows) = database
        .execute_sql("SELECT * FROM values WHERE value = NULL", &[])
        .unwrap()
    else {
        panic!("SELECT must return rows")
    };
    assert_eq!(null_rows.len(), 1);
    assert_eq!(null_rows[0].values.get("value"), Some(&Value::Null));

    let SqlResult::Rows(not_one_rows) = database
        .execute_sql("SELECT * FROM values WHERE value != 1", &[])
        .unwrap()
    else {
        panic!("SELECT must return rows")
    };
    assert_eq!(not_one_rows.len(), 2);
}
