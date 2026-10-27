use std::{
    fs,
    path::PathBuf,
    process::{Command, ExitStatus},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use seaworld_db::Database;

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

fn kill_writer(temp: &TempDatabase, point: &str) -> ExitStatus {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
        .arg(&temp.path)
        .env("SEAWORLD_CRASH_AT", point)
        .env("SEAWORLD_CRASH_READY_FILE", &temp.ready_path)
        .spawn()
        .unwrap();
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
