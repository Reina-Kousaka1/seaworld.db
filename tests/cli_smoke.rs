// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use seaworld_db::{Database, Document, Value};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "seaworld-cli-{}-{id}.seaworld.db",
            std::process::id()
        )))
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut lock = self.0.as_os_str().to_os_string();
        lock.push(".writer.lock");
        let _ = fs::remove_file(PathBuf::from(lock));
    }
}

#[test]
fn shell_inspection_check_backup_and_restore_smoke() {
    let source = TempDatabase::new();
    let backup = TempDatabase::new();
    let restored = TempDatabase::new();
    let mut database = Database::create(&source.0).unwrap();
    database.create_collection("users").unwrap();
    database
        .insert(
            "users",
            Document::empty().with("name", Value::String("Ada".into())),
        )
        .unwrap();
    database.close().unwrap();

    let mut shell = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg(&source.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    shell
        .stdin
        .take()
        .unwrap()
        .write_all(b".info\n.collections\n.indexes\n.schema\n.check\nSELECT * FROM users;\n.quit\n")
        .unwrap();
    let output = shell.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("SeaWorld DB"));
    assert!(stdout.contains("Format Version: 4"));
    assert!(stdout.contains("COLLECTION users"));
    assert!(stdout.contains("Ada"));

    let checked = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("check")
        .arg(&source.0)
        .output()
        .unwrap();
    assert!(checked.status.success());
    assert!(
        String::from_utf8(checked.stdout)
            .unwrap()
            .contains("Integrity check passed")
    );

    assert!(
        Command::new(env!("CARGO_BIN_EXE_seaworld"))
            .arg("backup")
            .arg(&source.0)
            .arg(&backup.0)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(env!("CARGO_BIN_EXE_seaworld"))
            .arg("verify")
            .arg(&backup.0)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(env!("CARGO_BIN_EXE_seaworld"))
            .arg("restore")
            .arg(&backup.0)
            .arg(&restored.0)
            .status()
            .unwrap()
            .success()
    );
    let restored = Database::open_reader(&restored.0).unwrap();
    assert_eq!(restored.document_ids("users").unwrap().len(), 1);
}

#[test]
fn explicit_open_and_create_modes_guard_against_mistyped_paths() {
    let missing = TempDatabase::new();
    let output = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("--open")
        .arg(&missing.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!missing.0.exists(), "--open must never create a database");

    let existing = TempDatabase::new();
    Database::create(&existing.0).unwrap().close().unwrap();
    let before = fs::read(&existing.0).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("--create")
        .arg(&existing.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&existing.0).unwrap(), before);

    let opened = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("--open")
        .arg(&existing.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(opened.status.success());

    // The backward-compatible default still creates, but says so.
    let implicit = TempDatabase::new();
    let output = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg(&implicit.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(implicit.0.exists());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("created new database")
    );
}

#[test]
fn usage_and_verified_compact_commands_report_without_changing_the_source() {
    let source = TempDatabase::new();
    let compacted = TempDatabase::new();
    let mut database = Database::create(&source.0).unwrap();
    database.create_collection("users").unwrap();
    database.create_index("users", "score").unwrap();
    let id = database
        .insert("users", Document::empty().with("score", Value::Integer(1)))
        .unwrap();
    for score in 2..40 {
        database
            .update(
                "users",
                id,
                Document::empty().with("score", Value::Integer(score)),
            )
            .unwrap();
    }
    database.close().unwrap();
    let before = fs::read(&source.0).unwrap();

    let usage = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("usage")
        .arg(&source.0)
        .output()
        .unwrap();
    assert!(usage.status.success());
    let stdout = String::from_utf8(usage.stdout).unwrap();
    assert!(stdout.contains(&format!("File bytes: {}", before.len())));
    assert!(stdout.contains("Estimated live bytes (estimate):"));
    assert!(stdout.contains("Compaction recommended:"));

    let compact = Command::new(env!("CARGO_BIN_EXE_seaworld"))
        .arg("compact")
        .arg(&source.0)
        .arg(&compacted.0)
        .output()
        .unwrap();
    assert!(compact.status.success());
    assert!(
        String::from_utf8(compact.stdout)
            .unwrap()
            .contains("Verified compacted copy")
    );
    assert_eq!(fs::read(&source.0).unwrap(), before);
    let copy = Database::open_reader(&compacted.0).unwrap();
    assert_eq!(
        copy.get("users", id).unwrap(),
        Some(&Document::empty().with("score", Value::Integer(39)))
    );
    assert!(fs::metadata(&compacted.0).unwrap().len() < before.len() as u64);
}
