// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{Database, Document, DocumentId, SnapshotExport, Value};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "seaworld-snapshot-export-{}-{id}.db",
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

fn collect(export: &SnapshotExport, collection: &str) -> Vec<DocumentId> {
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let rows = export.list_documents(collection, after, 1).unwrap();
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            ids.push(row.key);
            after = Some(row.key);
        }
    }
    ids
}

#[test]
fn snapshot_export_is_pinned_complete_and_detects_source_drift() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("molly_g_1").unwrap();
    db.create_collection("molly_global_settings").unwrap();
    let first = db
        .insert(
            "molly_g_1",
            Document::empty()
                .with("guild", Value::String("1".into()))
                .with("value", Value::Integer(1)),
        )
        .unwrap();

    let deleted_before_snapshot = db
        .insert(
            "molly_g_1",
            Document::empty().with("value", Value::Integer(-1)),
        )
        .unwrap();
    assert!(db.delete("molly_g_1", deleted_before_snapshot).unwrap());

    let export = db.begin_snapshot_export().unwrap();
    let descriptor = export.descriptor().clone();
    assert!(descriptor.inventory_complete);
    assert_eq!(descriptor.inventory_scope, "all_logical_collections");
    assert!(descriptor.snapshot_id.starts_with("sw:"));
    let inventory = export.inventory().unwrap();
    assert_eq!(inventory.record_count, 1);
    assert_eq!(inventory.collections.len(), 2);
    assert_eq!(inventory.collections[0].name, "molly_g_1");
    assert_eq!(inventory.collections[0].document_count, 1);
    assert_eq!(inventory.collections[1].name, "molly_global_settings");
    assert_eq!(inventory.collections[1].document_count, 0);
    assert_eq!(inventory.sha256.len(), 64);
    assert_eq!(export.inventory().unwrap(), inventory);

    let page = export.list_documents("molly_g_1", None, 10).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].key, first);
    assert!(page[0].serialized_bytes > 0);
    assert_eq!(page[0].sha256.len(), 64);
    assert_eq!(collect(&export, "molly_g_1"), vec![first]);

    db.insert(
        "molly_g_1",
        Document::empty()
            .with("guild", Value::String("1".into()))
            .with("value", Value::Integer(2)),
    )
    .unwrap();
    assert!(db.delete("molly_g_1", first).unwrap());
    db.create_collection("molly_g_2").unwrap();

    assert!(!export.is_current(&db).unwrap());
    assert_eq!(collect(&export, "molly_g_1"), vec![first]);
    assert_eq!(export.list_collections().unwrap().len(), 2);
    let current = db.begin_snapshot_export().unwrap();
    assert_ne!(current.inventory().unwrap().sha256, inventory.sha256);
    assert_eq!(current.list_collections().unwrap().len(), 3);
    assert_eq!(current.inventory().unwrap().record_count, 1);
}

#[test]
fn snapshot_export_rejects_invalid_page_sizes_and_unknown_collections() {
    let temp = TempDatabase::new();
    let mut db = Database::create(&temp.0).unwrap();
    db.create_collection("known").unwrap();
    let export = db.begin_snapshot_export().unwrap();
    assert!(export.list_documents("known", None, 0).is_err());
    assert!(export.list_documents("known", None, 1_001).is_err());
    assert!(export.list_documents("missing", None, 1).is_err());
}
