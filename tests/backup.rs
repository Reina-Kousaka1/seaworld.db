// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{
    CompareOp, Database, Document, DocumentId, FieldPath, Filter, Query, QueryPlan, Value,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("seaworld-backup-{}-{id}", std::process::id()));
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

fn item(score: i64, name: &str) -> Document {
    Document::empty()
        .with("score", Value::Integer(score))
        .with("name", Value::String(name.into()))
}

fn rows(database: &Database, query: &Query) -> Vec<(i64, Document)> {
    database
        .execute(query)
        .unwrap()
        .into_iter()
        .map(|row| (row.id.value(), (*row.document).clone()))
        .collect()
}

fn index_metadata(database: &Database) -> Vec<(i64, String, String, bool)> {
    database
        .indexes()
        .unwrap()
        .into_iter()
        .map(|index| {
            (
                index.id.value(),
                index.collection,
                index.path.to_string(),
                index.unique,
            )
        })
        .collect()
}

fn indexed_and_scan_queries() -> (Query, Query) {
    let filter = Filter::Compare {
        path: FieldPath::parse("score").unwrap(),
        op: CompareOp::Gte,
        value: Value::Integer(10),
    };
    (
        Query::new("users", filter.clone()),
        Query::new(
            "users",
            Filter::Not(Box::new(Filter::Not(Box::new(filter)))),
        ),
    )
}

#[test]
fn backup_during_staged_transaction_restores_committed_state_and_watermarks() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let backup = dir.path("backup.seaworld.db");
    let restored = dir.path("restored.seaworld.db");
    let compacted = dir.path("compacted.seaworld.db");
    let existing = dir.path("existing.seaworld.db");
    let mut writer = Database::create(&source).unwrap();
    writer.create_collection("users").unwrap();
    let first = writer.insert("users", item(10, "a")).unwrap();
    let deleted = writer.insert("users", item(20, "b")).unwrap();
    assert!(writer.delete("users", deleted).unwrap());
    writer.create_index("users", "score").unwrap();
    writer.create_unique_index("users", "name").unwrap();
    let identity = writer.database_id();
    let version = writer.current_version().unwrap().value();
    let indexes = index_metadata(&writer);
    let source_bytes = fs::read(&source).unwrap();

    let mut pending = writer.begin_transaction().unwrap();
    assert!(pending.update("users", first, item(100, "a")).unwrap());
    pending.insert("users", item(30, "c")).unwrap();
    let reader = Database::open_reader(&source).unwrap();
    reader.backup(&backup).unwrap();
    reader.close().unwrap();
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    pending.commit().unwrap();

    let report = Database::verify_backup(&backup).unwrap();
    assert_eq!(report.database_id, identity);
    assert_eq!(report.format_version, 4);
    assert_eq!(report.document_count, 1);
    assert_eq!(report.index_count, 2);
    assert!(report.committed_version >= version);
    let backup_db = Database::open_reader(&backup).unwrap();
    assert_eq!(backup_db.get("users", first).unwrap(), Some(&item(10, "a")));
    assert_eq!(index_metadata(&backup_db), indexes);
    let (indexed, scan) = indexed_and_scan_queries();
    assert!(matches!(
        backup_db.explain(&indexed).unwrap(),
        QueryPlan::SecondaryIndexRangeLookup { .. }
    ));
    assert!(matches!(
        backup_db.explain(&scan).unwrap(),
        QueryPlan::FullScan { .. }
    ));
    assert_eq!(rows(&backup_db, &indexed), rows(&backup_db, &scan));
    let expected = rows(&backup_db, &Query::new("users", Filter::All));
    backup_db.close().unwrap();

    fs::write(&existing, b"preserve existing target").unwrap();
    assert!(Database::restore_backup(&backup, &existing).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"preserve existing target");
    Database::restore_backup(&backup, &restored).unwrap();
    let mut restored_db = Database::open(&restored).unwrap();
    assert_eq!(restored_db.database_id(), identity);
    assert_eq!(index_metadata(&restored_db), indexes);
    assert_eq!(
        rows(&restored_db, &Query::new("users", Filter::All)),
        expected
    );
    assert_eq!(rows(&restored_db, &indexed), rows(&restored_db, &scan));
    assert!(restored_db.current_version().unwrap().value() >= version);
    let next = restored_db.insert("users", item(99, "z")).unwrap();
    assert!(next.value() > deleted.value());
    restored_db.write_compacted_copy(&compacted).unwrap();
    restored_db.close().unwrap();
    let compacted_db = Database::open_reader(&compacted).unwrap();
    assert_eq!(
        compacted_db.get("users", next).unwrap(),
        Some(&item(99, "z"))
    );
    compacted_db.close().unwrap();
    writer.close().unwrap();
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn seal_header(bytes: &mut [u8]) {
    let checksum = crc32c(&bytes[..60]);
    bytes[60..64].copy_from_slice(&checksum.to_be_bytes());
}

#[test]
fn corrupt_backups_fail_verification_and_cannot_damage_restore_targets() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let backup = dir.path("backup.seaworld.db");
    let mut database = Database::create(&source).unwrap();
    database.create_collection("users").unwrap();
    database.insert("users", item(10, "a")).unwrap();
    database.create_index("users", "score").unwrap();
    database.backup(&backup).unwrap();
    database.close().unwrap();
    let original = fs::read(&backup).unwrap();
    let mut variants = Vec::new();
    let mut bad_magic = original.clone();
    bad_magic[0] ^= 1;
    variants.push(bad_magic);
    let mut bad_header_checksum = original.clone();
    bad_header_checksum[60] ^= 1;
    variants.push(bad_header_checksum);
    let mut unsupported_version = original.clone();
    unsupported_version[8..10].copy_from_slice(&999_u16.to_be_bytes());
    seal_header(&mut unsupported_version);
    variants.push(unsupported_version);
    let mut unknown_flags = original.clone();
    unknown_flags[16] = 1;
    seal_header(&mut unknown_flags);
    variants.push(unknown_flags);
    let mut bad_length = original.clone();
    bad_length[72..76].copy_from_slice(&i32::MAX.to_be_bytes());
    variants.push(bad_length);
    let mut bad_checksum = original.clone();
    *bad_checksum.last_mut().unwrap() ^= 1;
    variants.push(bad_checksum);
    let mut bad_page = original.clone();
    let mut offset = 64_usize;
    let mut found_page = false;
    while offset < bad_page.len() {
        let length_bytes = &bad_page[offset + 8..offset + 12];
        let length = u32::from_be_bytes(length_bytes.try_into().unwrap()) as usize;
        if bad_page[offset + 4] == 10 && length == 4_120 {
            bad_page[offset + 20 + 4_091] ^= 1;
            let checksum_offset = offset + length - 4;
            let checksum = crc32c(&bad_page[offset..checksum_offset]);
            bad_page[checksum_offset..offset + length].copy_from_slice(&checksum.to_be_bytes());
            found_page = true;
            break;
        }
        offset += length;
    }
    assert!(found_page, "backup must contain a standalone index page");
    variants.push(bad_page);
    let mut truncated = original.clone();
    truncated.pop();
    variants.push(truncated);
    let mut trailing = original.clone();
    trailing.extend_from_slice(b"SWB");
    variants.push(trailing);

    let existing = dir.path("existing.seaworld.db");
    fs::write(&existing, b"untouched").unwrap();
    for (index, bytes) in variants.iter().enumerate() {
        let corrupted = dir.path(&format!("corrupt-{index}.seaworld.db"));
        let destination = dir.path(&format!("restore-{index}.seaworld.db"));
        fs::write(&corrupted, bytes).unwrap();
        assert!(Database::verify_backup(&corrupted).is_err(), "case {index}");
        assert!(
            Database::restore_backup(&corrupted, &destination).is_err(),
            "case {index}"
        );
        assert!(!destination.exists(), "case {index}");
        assert!(
            Database::restore_backup(&corrupted, &existing).is_err(),
            "case {index}"
        );
        assert_eq!(fs::read(&existing).unwrap(), b"untouched");
        assert_eq!(fs::read(&corrupted).unwrap(), *bytes);
    }
}

#[test]
fn java_v1_fixture_can_be_verified_and_restored_without_changing_it() {
    let dir = TestDir::new();
    let source = dir.path("java-v1.seaworld.db");
    let restored = dir.path("restored.seaworld.db");
    let fixture = include_bytes!("fixtures/java-v1-basic.fixture");
    fs::write(&source, fixture).unwrap();
    let report = Database::verify_backup(&source).unwrap();
    assert_eq!(report.format_version, 1);
    Database::restore_backup(&source, &restored).unwrap();
    let restored_db = Database::open_reader(&restored).unwrap();
    assert_eq!(restored_db.format_version(), 4);
    assert_eq!(restored_db.database_id(), report.database_id);
    assert_eq!(
        restored_db.document_ids("users").unwrap(),
        [DocumentId::new(42).unwrap()]
    );
    assert_eq!(fs::read(&source).unwrap(), fixture);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 8,
        max_shrink_iters: 1_000,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn generated_snapshot_and_backup_restore_match_committed_model(
        operations in proptest::collection::vec(
            (proptest::prelude::any::<bool>(), 0_u8..=2, 1_i64..=6, -3_i64..=12),
            0..=12
        )
    ) {
        let dir = TestDir::new();
        let source = dir.path("source.seaworld.db");
        let backup = dir.path("backup.seaworld.db");
        let restored = dir.path("restored.seaworld.db");
        let mut writer = Database::create(&source).unwrap();
        writer.create_collection("users").unwrap();
        writer.insert_with_id("users", DocumentId::new(1).unwrap(), item(10, "user-1")).unwrap();
        writer.create_index("users", "score").unwrap();
        writer.create_unique_index("users", "name").unwrap();
        let identity = writer.database_id();
        let snapshot = writer.begin_read().unwrap();
        let all = Query::new("users", Filter::All);
        let initial: Vec<_> = snapshot.execute(&all).unwrap().into_iter()
            .map(|row| (row.id.value(), row.document)).collect();
        let mut model = BTreeMap::from([(1_i64, 10_i64)]);

        for (commit, kind, raw_id, score) in operations {
            let id = DocumentId::new(raw_id).unwrap();
            let name = format!("user-{raw_id}");
            let mut staged = model.clone();
            let mut transaction = writer.begin_transaction().unwrap();
            match kind {
                0 if !staged.contains_key(&raw_id) => {
                    transaction.insert_with_id("users", id, item(score, &name)).unwrap();
                    staged.insert(raw_id, score);
                }
                1 if staged.contains_key(&raw_id) => {
                    assert!(transaction.update("users", id, item(score, &name)).unwrap());
                    staged.insert(raw_id, score);
                }
                2 if staged.contains_key(&raw_id) => {
                    assert!(transaction.delete("users", id).unwrap());
                    staged.remove(&raw_id);
                }
                _ => {}
            }
            if commit {
                transaction.commit().unwrap();
                model = staged;
            } else {
                transaction.rollback();
            }
            let observed: Vec<_> = snapshot.execute(&all).unwrap().into_iter()
                .map(|row| (row.id.value(), row.document)).collect();
            proptest::prop_assert_eq!(&observed, &initial);
        }

        drop(snapshot);
        let expected: Vec<_> = model.iter()
            .map(|(id, score)| (*id, item(*score, &format!("user-{id}"))))
            .collect();
        let (indexed, scan) = indexed_and_scan_queries();
        proptest::prop_assert_eq!(&rows(&writer, &all), &expected);
        proptest::prop_assert_eq!(rows(&writer, &indexed), rows(&writer, &scan));
        let source_version = writer.current_version().unwrap().value();
        writer.backup(&backup).unwrap();
        writer.close().unwrap();
        let report = Database::verify_backup(&backup).unwrap();
        proptest::prop_assert_eq!(report.database_id, identity);
        proptest::prop_assert!(report.committed_version >= source_version);
        proptest::prop_assert_eq!(report.index_count, 2);
        Database::restore_backup(&backup, &restored).unwrap();
        let restored_db = Database::open_reader(&restored).unwrap();
        proptest::prop_assert_eq!(&rows(&restored_db, &all), &expected);
        proptest::prop_assert_eq!(rows(&restored_db, &indexed), rows(&restored_db, &scan));
        proptest::prop_assert_eq!(restored_db.database_id(), identity);
        proptest::prop_assert_eq!(restored_db.indexes().unwrap().len(), 2);
    }
}

#[cfg(feature = "crash-test")]
#[test]
fn interrupted_backup_never_publishes_a_partial_destination() {
    use std::{
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let prior = dir.path("prior.seaworld.db");
    let mut database = Database::create(&source).unwrap();
    database.create_collection("users").unwrap();
    database.insert("users", item(10, "a")).unwrap();
    database.backup(&prior).unwrap();
    database.close().unwrap();
    let prior_bytes = fs::read(&prior).unwrap();

    for (index, point) in [
        "compaction_after_create",
        "transaction_during_first_operation",
        "compaction_before_publish",
    ]
        .iter()
        .enumerate()
    {
        let target = dir.path(&format!("interrupted-{index}.seaworld.db"));
        let ready = dir.path(&format!("ready-{index}"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
            .arg(&source)
            .arg("--backup")
            .arg(&target)
            .env("SEAWORLD_CRASH_AT", point)
            .env("SEAWORLD_CRASH_READY_FILE", &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("backup process exited before {point}: {status}");
            }
            if ready.exists() {
                child.kill().unwrap();
                child.wait().unwrap();
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("backup process did not reach {point}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!target.exists());
        assert_eq!(fs::read(&prior).unwrap(), prior_bytes);
        Database::verify_backup(&prior).unwrap();
        let source_db = Database::open_reader(&source).unwrap();
        assert_eq!(source_db.document_ids("users").unwrap().len(), 1);
    }
}
