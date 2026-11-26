// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
//! Verified compaction workflow: data equality, refusal paths, process kills
//! and injected faults. Process kills do not simulate power loss.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use seaworld_db::{Database, Document, Error, Filter, Query, Value};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("seaworld-compact-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
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

fn rows(database: &Database) -> Vec<(i64, Document)> {
    database
        .execute(&Query::new("users", Filter::All))
        .unwrap()
        .into_iter()
        .map(|row| (row.id.value(), (*row.document).clone()))
        .collect()
}

/// Builds a source with updates, deletes, an index and obsolete COW pages.
fn populate(mut database: Database) -> Vec<(i64, Document)> {
    database.create_collection("users").unwrap();
    database.create_collection("empty").unwrap();
    let mut ids = Vec::new();
    for score in 0..40 {
        ids.push(
            database
                .insert("users", item(score, &format!("u{score}")))
                .unwrap(),
        );
    }
    if database.format_version() >= 2 {
        database.create_index("users", "score").unwrap();
        database.create_unique_index("users", "name").unwrap();
    }
    for (position, id) in ids.iter().enumerate() {
        if position % 3 == 0 {
            assert!(database.delete("users", *id).unwrap());
        } else {
            let name = format!("u{position}");
            assert!(
                database
                    .update("users", *id, item(500 + position as i64, &name))
                    .unwrap()
            );
        }
    }
    let expected = rows(&database);
    database.close().unwrap();
    expected
}

#[test]
fn verified_compaction_preserves_data_for_every_format_version() {
    for version in 1..=4_u16 {
        let dir = TestDir::new();
        let source = dir.path("source.seaworld.db");
        let target = dir.path("compacted.seaworld.db");
        let database = match version {
            1 => Database::create_v1(&source),
            2 => Database::create_v2(&source),
            3 => Database::create_v3(&source),
            _ => Database::create(&source),
        }
        .unwrap();
        let expected = populate(database);
        let source_bytes = fs::read(&source).unwrap();

        let database = Database::open(&source).unwrap();
        let identity = database.database_id();
        let report = database.compact_verified(&target).unwrap();
        assert_eq!(report.format_version, version, "V{version}");
        assert_eq!(report.integrity.database_id, identity);
        assert_eq!(report.integrity.collection_count, 2);
        assert_eq!(report.integrity.document_count, expected.len());
        assert_eq!(report.source_file_bytes, source_bytes.len() as u64);
        assert!(report.compacted_file_bytes <= report.source_file_bytes);
        assert_eq!(report.path, fs::canonicalize(&target).unwrap());
        database.close().unwrap();
        assert_eq!(fs::read(&source).unwrap(), source_bytes, "V{version}");

        let copy = Database::open(&report.path).unwrap();
        assert_eq!(copy.format_version(), version);
        assert_eq!(rows(&copy), expected, "V{version}");
        copy.check_integrity().unwrap();
        // No temporary siblings or writer-lock files of the copy survive.
        assert!(
            dir.entries()
                .iter()
                .all(|name| !name.contains(".swdb-tmp-")),
            "{:?}",
            dir.entries()
        );
    }
}

#[test]
fn verified_compaction_refuses_existing_or_source_targets_without_touching_them() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let existing = dir.path("existing.seaworld.db");
    populate(Database::create(&source).unwrap());
    let source_bytes = fs::read(&source).unwrap();
    // A truncated, incomplete database left where the target should go.
    fs::write(&existing, &source_bytes[..source_bytes.len() / 2]).unwrap();

    let database = Database::open(&source).unwrap();
    assert!(matches!(
        database.compact_verified(&existing),
        Err(Error::Duplicate(_))
    ));
    assert!(matches!(
        database.compact_verified(&source),
        Err(Error::InvalidInput(_))
    ));
    database.close().unwrap();
    assert_eq!(
        fs::read(&existing).unwrap(),
        &source_bytes[..source_bytes.len() / 2]
    );
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
}

#[test]
fn stale_or_corrupt_temporary_files_are_neither_used_nor_published() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let target = dir.path("target.seaworld.db");
    let expected = populate(Database::create(&source).unwrap());
    // Leftovers that look like an interrupted compaction of the same target.
    let stale = [
        dir.path("target.seaworld.db.swdb-tmp-1-0"),
        dir.path(&format!(
            "target.seaworld.db.swdb-tmp-{}-0",
            std::process::id()
        )),
    ];
    for path in &stale {
        fs::write(path, b"SWDB\r\n\x1a\ngarbage").unwrap();
    }
    let database = Database::open(&source).unwrap();
    let report = database.compact_verified(&target).unwrap();
    database.close().unwrap();
    assert_eq!(
        rows(&Database::open_reader(&report.path).unwrap()),
        expected
    );
    for path in &stale {
        assert_eq!(fs::read(path).unwrap(), b"SWDB\r\n\x1a\ngarbage");
        assert!(Database::open_reader(path).is_err());
    }
}

#[test]
fn concurrent_compactions_to_one_target_publish_exactly_one_verified_copy() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let target = dir.path("target.seaworld.db");
    let expected = populate(Database::create(&source).unwrap());
    let writer = Database::open(&source).unwrap();
    assert!(matches!(Database::open(&source), Err(Error::Locked)));

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let source = source.clone();
            let target = target.clone();
            thread::spawn(move || {
                let reader = Database::open_reader(&source).unwrap();
                reader.compact_verified(&target).map(|report| report.path)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let successes = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(successes, 1, "{results:?}");
    for result in &results {
        if let Err(error) = result {
            assert!(
                matches!(error, Error::Duplicate(_))
                    || matches!(error, Error::Io(io) if io.kind() == std::io::ErrorKind::AlreadyExists),
                "{error}"
            );
        }
    }
    drop(writer);
    let copy = Database::open_reader(&target).unwrap();
    copy.check_integrity().unwrap();
    assert_eq!(rows(&copy), expected);
}

struct Outcome {
    killed: bool,
    exit_code: Option<i32>,
}

fn run_compact_verified(
    source: &Path,
    target: &Path,
    ready: &Path,
    crash_at: Option<&str>,
    fail_at: Option<&str>,
) -> Outcome {
    let _ = fs::remove_file(ready);
    let mut command = Command::new(env!("CARGO_BIN_EXE_crash_writer"));
    command.arg(source).arg("--compact-verified").arg(target);
    if let Some(point) = crash_at {
        command
            .env("SEAWORLD_CRASH_AT", point)
            .env("SEAWORLD_CRASH_READY_FILE", ready);
    }
    if let Some(point) = fail_at {
        command.env("SEAWORLD_FAIL_AT", point);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Outcome {
                killed: false,
                exit_code: status.code(),
            };
        }
        if crash_at.is_some() && ready.exists() {
            child.kill().unwrap();
            child.wait().unwrap();
            return Outcome {
                killed: true,
                exit_code: None,
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("verified compaction did not finish or reach {crash_at:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_source_intact(source: &Path, source_bytes: &[u8], expected: &[(i64, Document)]) {
    assert_eq!(fs::read(source).unwrap(), source_bytes);
    let reader = Database::open_reader(source).unwrap();
    reader.check_integrity().unwrap();
    assert_eq!(rows(&reader), expected);
}

fn assert_valid_copy(target: &Path, expected: &[(i64, Document)]) {
    let copy = Database::open_reader(target).unwrap();
    copy.check_integrity().unwrap();
    assert_eq!(rows(&copy), expected);
}

#[test]
fn killed_verified_compaction_never_publishes_a_partial_copy_or_changes_the_source() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let expected = populate(Database::create(&source).unwrap());
    let source_bytes = fs::read(&source).unwrap();

    for (index, (point, published)) in [
        ("compaction_after_create", false),
        // Inside the copy's document batch (during the compaction write).
        ("transaction_between_operations", false),
        // The copy's commit is written but not yet synchronized.
        ("transaction_after_commit_write_before_sync", false),
        // The copy's commit is synchronized but compaction is unfinished.
        ("transaction_after_sync_before_return", false),
        ("compaction_before_publish", false),
        ("compaction_after_publish_before_directory_sync", true),
        ("compaction_after_directory_sync_before_cleanup", true),
        ("verified_compaction_after_publish_before_verify", true),
    ]
    .into_iter()
    .enumerate()
    {
        let target = dir.path(&format!("target-{index}.seaworld.db"));
        let ready = dir.path(&format!("ready-{index}"));
        let outcome = run_compact_verified(&source, &target, &ready, Some(point), None);
        assert!(outcome.killed, "{point} was not reached");
        if published {
            assert_valid_copy(&target, &expected);
        } else {
            assert!(!target.exists(), "partial target published at {point}");
        }
        assert_source_intact(&source, &source_bytes, &expected);

        // A rerun to a fresh target succeeds despite any leftover temporary.
        let rerun = dir.path(&format!("rerun-{index}.seaworld.db"));
        let outcome = run_compact_verified(&source, &rerun, &ready, None, None);
        assert_eq!(outcome.exit_code, Some(0), "rerun after {point}");
        assert_valid_copy(&rerun, &expected);
        assert_source_intact(&source, &source_bytes, &expected);
    }
}

#[test]
fn injected_directory_sync_failure_reports_an_error_and_keeps_the_source() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let target = dir.path("target.seaworld.db");
    let ready = dir.path("ready");
    let expected = populate(Database::create(&source).unwrap());
    let source_bytes = fs::read(&source).unwrap();
    let outcome = run_compact_verified(
        &source,
        &target,
        &ready,
        None,
        Some("compaction_directory_sync"),
    );
    assert_eq!(outcome.exit_code, Some(9));
    // The hard link may exist; its durability is uncertain, but it is complete.
    if target.exists() {
        assert_valid_copy(&target, &expected);
    }
    assert_source_intact(&source, &source_bytes, &expected);
    assert!(
        dir.entries()
            .iter()
            .all(|name| !name.contains(".swdb-tmp-")),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn injected_reopen_failure_is_an_error_and_never_switches_or_deletes() {
    let dir = TestDir::new();
    let source = dir.path("source.seaworld.db");
    let target = dir.path("target.seaworld.db");
    let ready = dir.path("ready");
    let expected = populate(Database::create(&source).unwrap());
    let source_bytes = fs::read(&source).unwrap();
    let outcome = run_compact_verified(
        &source,
        &target,
        &ready,
        None,
        Some("verified_compaction_reopen"),
    );
    assert_eq!(outcome.exit_code, Some(9));
    assert_source_intact(&source, &source_bytes, &expected);
    // Verification did not succeed, so the caller got no report; the
    // published copy is left for inspection rather than deleted.
    assert_valid_copy(&target, &expected);
}

#[test]
fn killed_batched_index_creation_leaves_no_partial_index() {
    let dir = TestDir::new();
    let source = dir.path("index.seaworld.db");
    {
        let mut database = Database::create(&source).unwrap();
        database.create_collection("users").unwrap();
        // Enough documents for a multi-page tree in one batched page append.
        for score in 0..600 {
            database
                .insert("users", item(score, &format!("user-{score:05}")))
                .unwrap();
        }
        database.close().unwrap();
    }
    let expected = rows(&Database::open_reader(&source).unwrap());

    for (index, point) in [
        "before_write",
        "during_write",
        "after_write_before_sync",
        "after_sync_before_return",
        "index_create_after_pages_before_definition",
    ]
    .into_iter()
    .enumerate()
    {
        let ready = dir.path(&format!("ready-index-{index}"));
        let _ = fs::remove_file(&ready);
        let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
            .arg(&source)
            .arg("--create-index")
            .env("SEAWORLD_CRASH_AT", point)
            .env("SEAWORLD_CRASH_READY_FILE", &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("index creation exited before {point}: {status}");
            }
            if ready.exists() {
                child.kill().unwrap();
                child.wait().unwrap();
                break;
            }
            assert!(Instant::now() < deadline, "{point} was not reached");
            thread::sleep(Duration::from_millis(10));
        }
        // Writable recovery: no index definition, documents unchanged, any
        // completed pages are unreachable, and recovery is idempotent.
        let database = Database::open(&source).unwrap();
        assert!(
            database.indexes().unwrap().is_empty(),
            "index after {point}"
        );
        assert_eq!(rows(&database), expected);
        database.check_integrity().unwrap();
        database.close().unwrap();
        let reopened = Database::open(&source).unwrap();
        assert_eq!(reopened.recovery_report().discarded_bytes(), 0);
        reopened.close().unwrap();
    }

    let mut database = Database::open(&source).unwrap();
    database.create_index("users", "score").unwrap();
    database.check_integrity().unwrap();
    database.close().unwrap();
    let reopened = Database::open_reader(&source).unwrap();
    assert_eq!(reopened.indexes().unwrap().len(), 1);
    reopened.check_integrity().unwrap();
    assert_eq!(rows(&reopened), expected);
}
