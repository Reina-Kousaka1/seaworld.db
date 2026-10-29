// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{Database, Document, Error};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seaworld-fuzz-recovery-{}-{id}.seaworld.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(format!("{}.writer.lock", self.0.display()));
    }
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

fn append_record(path: &PathBuf, kind: u8, sequence: i64, payload: &[u8]) {
    let length = 24 + payload.len();
    let mut record = Vec::with_capacity(length);
    record.extend_from_slice(b"SWBL");
    record.push(kind);
    record.push(0);
    record.extend_from_slice(&0_u16.to_be_bytes());
    record.extend_from_slice(&(length as i32).to_be_bytes());
    record.extend_from_slice(&sequence.to_be_bytes());
    record.extend_from_slice(payload);
    record.extend_from_slice(&crc32c(&record).to_be_bytes());
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&record)
        .unwrap();
}

use std::io::Write;

fn assert_open_never_panics(path: &PathBuf) {
    let result = std::panic::catch_unwind(|| Database::open(path));
    assert!(result.is_ok(), "opening {:?} panicked", path);
    if let Ok(Ok(database)) = result {
        database.close().unwrap();
    }
}

#[test]
fn checksummed_semantic_transaction_corruption_fails_closed() {
    let cases: &[(&str, &[(u8, i64)])] = &[
        ("commit without begin", &[(6, 1)]),
        ("nested begin", &[(5, 1), (5, 2)]),
        ("mismatched commit", &[(5, 1), (6, 2)]),
        ("zero transaction id", &[(5, 0)]),
        ("unknown record", &[(255, 1)]),
    ];
    for (name, records) in cases {
        let temp = TempDatabase::new();
        Database::create(&temp.0).unwrap().close().unwrap();
        for (index, (kind, id)) in records.iter().enumerate() {
            append_record(&temp.0, *kind, index as i64 + 1, &id.to_be_bytes());
        }
        let bytes = fs::read(&temp.0).unwrap();
        let outcome = std::panic::catch_unwind(|| Database::open(&temp.0));
        assert!(outcome.is_ok(), "{name} panicked");
        assert!(matches!(
            outcome.unwrap(),
            Err(Error::Integrity(_)) | Err(Error::Format(_))
        ));
        assert_eq!(fs::read(&temp.0).unwrap(), bytes, "{name} modified input");
    }
}

#[test]
fn complete_uncommitted_transaction_is_recovered_without_visibility() {
    let temp = TempDatabase::new();
    Database::create(&temp.0).unwrap().close().unwrap();
    append_record(&temp.0, 5, 1, &1_i64.to_be_bytes());
    let database = Database::open(&temp.0).unwrap();
    assert!(database.collection_names().unwrap().is_empty());
    assert!(database.recovery_report().discarded_bytes() > 0);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 48,
        max_shrink_iters: 1_000,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn arbitrary_bounded_database_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..=2048)
    ) {
        let temp = TempDatabase::new();
        fs::write(&temp.0, bytes).unwrap();
        assert_open_never_panics(&temp.0);
    }

    #[test]
    fn bounded_tails_preserve_an_already_committed_document(
        tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..=512)
    ) {
        let temp = TempDatabase::new();
        let mut database = Database::create(&temp.0).unwrap();
        database.create_collection("users").unwrap();
        let id = database.insert("users", Document::empty()).unwrap();
        database.close().unwrap();
        fs::OpenOptions::new().append(true).open(&temp.0).unwrap().write_all(&tail).unwrap();

        let result = std::panic::catch_unwind(|| Database::open(&temp.0));
        proptest::prop_assert!(result.is_ok(), "recovery panicked for tail length {}", tail.len());
        if let Ok(database) = result.unwrap() {
            proptest::prop_assert!(database.get("users", id).unwrap().is_some());
            database.close().unwrap();
        }
    }
}
