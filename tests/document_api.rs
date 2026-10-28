// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    Database, Document, Error, FieldPath, Filter, MAX_PROJECTION_FIELDS, Projection, Query, Value,
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
                .with("profile", Value::object([("level".into(), Value::Integer(1))]))
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
    assert_eq!(before, snapshot.execute_projected(&query, &projection).unwrap());
    assert_eq!(before[0].id, id);
    assert_eq!(before[0].fields.get(&path), Some(&Value::Integer(1)));
    assert_eq!(before[0].fields.get(&null_path), Some(&Value::Null));
    assert!(!before[0].fields.contains_key(&missing));
    db.update("items", id, Document::empty()).unwrap();
    assert_eq!(before, snapshot.execute_projected(&query, &projection).unwrap());
    assert!(db.execute_projected(&query, &projection).unwrap()[0].fields.is_empty());
    assert!(matches!(
        Projection::new(std::iter::repeat_n(path, MAX_PROJECTION_FIELDS + 1)),
        Err(Error::Limit(_))
    ));
}
