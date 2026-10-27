use std::{
    fs,
    path::PathBuf,
    process::{Command, ExitStatus},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use seaworld_db::{Database, DocumentId};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase {
    path: PathBuf,
    ready_path: PathBuf,
}

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "seaworld-crash-{}-{id}.seaworld.db",
            std::process::id()
        ));
        let ready_path = path.with_extension("ready");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&ready_path);
        Self { path, ready_path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.ready_path);
    }
}

fn kill_process(temp: &TempDatabase, point: &str, recovery_mode: bool) -> ExitStatus {
    let _ = fs::remove_file(&temp.ready_path);
    let mut command = Command::new(env!("CARGO_BIN_EXE_crash_writer"));
    command
        .arg(&temp.path)
        .env("SEAWORLD_CRASH_AT", point)
        .env("SEAWORLD_CRASH_READY_FILE", &temp.ready_path);
    if recovery_mode {
        command.arg("--recover");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("crash writer exited before the injected stop point: {status}");
        }
        if temp.ready_path.exists() {
            child.kill().unwrap();
            return child.wait().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash writer did not reach {point:?} within 10 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn kill_writer(temp: &TempDatabase, point: &str) -> ExitStatus {
    kill_process(temp, point, false)
}

fn empty_database(path: &PathBuf) {
    Database::create(path).unwrap().close().unwrap();
}

#[test]
fn killed_before_append_keeps_database_unchanged() {
    let temp = TempDatabase::new();
    empty_database(temp.path());
    let status = kill_writer(&temp, "before_write");
    assert!(!status.success());
    let database = Database::open(temp.path()).unwrap();
    assert!(database.collection_names().unwrap().is_empty());
    assert_eq!(database.recovery_report().discarded_bytes(), 0);
}

#[test]
fn killed_during_block_write_recovers_the_incomplete_tail() {
    let temp = TempDatabase::new();
    empty_database(temp.path());
    let status = kill_writer(&temp, "during_write");
    assert!(!status.success());
    let database = Database::open(temp.path()).unwrap();
    assert!(database.collection_names().unwrap().is_empty());
    assert!(database.recovery_report().discarded_bytes() > 0);
    database.close().unwrap();
    let reopened = Database::open(temp.path()).unwrap();
    assert_eq!(reopened.recovery_report().discarded_bytes(), 0);
}

#[test]
fn killed_after_write_before_sync_leaves_a_valid_recoverable_state() {
    let temp = TempDatabase::new();
    empty_database(temp.path());
    let status = kill_writer(&temp, "after_write_before_sync");
    assert!(!status.success());
    let database = Database::open(temp.path()).unwrap();
    assert!(
        database.collection_names().unwrap().is_empty()
            || database.collection_names().unwrap() == ["users"]
    );
    database.close().unwrap();
    assert_eq!(
        Database::open(temp.path())
            .unwrap()
            .recovery_report()
            .discarded_bytes(),
        0
    );
}

#[test]
fn killed_after_sync_keeps_the_committed_operation() {
    let temp = TempDatabase::new();
    empty_database(temp.path());
    let status = kill_writer(&temp, "after_sync_before_return");
    assert!(!status.success());
    let database = Database::open(temp.path()).unwrap();
    assert_eq!(database.collection_names().unwrap(), ["users"]);
    assert_eq!(database.recovery_report().discarded_bytes(), 0);
}

#[test]
fn incomplete_v2_transactions_recover_without_partial_visibility() {
    for point in [
        "transaction_before_begin",
        "transaction_after_begin",
        "transaction_during_first_operation",
        "transaction_between_operations",
        "transaction_after_last_operation",
        "transaction_during_commit_record",
    ] {
        let temp = TempDatabase::new();
        empty_database(temp.path());
        assert!(!kill_writer(&temp, point).success(), "point {point}");
        let database = Database::open(temp.path()).unwrap();
        assert!(
            database.collection_names().unwrap().is_empty(),
            "partial transaction leaked a collection at {point}"
        );
        if point != "transaction_before_begin" {
            assert!(
                database.recovery_report().discarded_bytes() > 0,
                "recovery did not discard transaction suffix at {point}"
            );
        }
        assert!(database.document_ids("users").is_err());
        database.close().unwrap();
        let reopened = Database::open(temp.path()).unwrap();
        assert_eq!(reopened.recovery_report().discarded_bytes(), 0);
        assert!(reopened.collection_names().unwrap().is_empty());
    }
}

#[test]
fn complete_commit_marker_recovers_the_whole_transaction() {
    for point in [
        "transaction_after_commit_write_before_sync",
        "transaction_after_sync_before_return",
    ] {
        let temp = TempDatabase::new();
        empty_database(temp.path());
        assert!(!kill_writer(&temp, point).success(), "point {point}");
        let database = Database::open(temp.path()).unwrap();
        assert_eq!(database.collection_names().unwrap(), ["users"]);
        assert_eq!(
            database.document_ids("users").unwrap(),
            [DocumentId::new(1).unwrap(), DocumentId::new(2).unwrap()]
        );
    }
}

#[test]
fn recovery_can_be_interrupted_and_repeated() {
    let temp = TempDatabase::new();
    empty_database(temp.path());
    assert!(!kill_writer(&temp, "transaction_after_begin").success());
    assert!(!kill_process(&temp, "recovery_after_truncate_before_sync", true).success());
    let database = Database::open(temp.path()).unwrap();
    assert_eq!(database.recovery_report().discarded_bytes(), 0);
    assert!(database.collection_names().unwrap().is_empty());
}
