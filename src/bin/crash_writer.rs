// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{env, path::PathBuf, process};

use seaworld_db::{
    CompareOp, Database, Document, DocumentId, DocumentOperation, FieldPath, Filter, Query, Value,
};

fn path(value: &str) -> FieldPath {
    FieldPath::parse(value).expect("fixed field path")
}

fn main() {
    let mut arguments = env::args_os().skip(1);
    let Some(database_path) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: crash_writer <database-path> [--recover|--backup TARGET|--bulk-update|--bulk-delete|--upsert|--mixed-transaction]");
        process::exit(2);
    };
    let mode = arguments.next();
    let recovery_mode = mode.as_deref().is_some_and(|value| value == "--recover");
    let mut database = match Database::open(database_path) {
        Ok(database) => database,
        Err(error) => {
            eprintln!("could not open database: {error}");
            process::exit(3);
        }
    };
    if recovery_mode {
        if let Err(error) = database.close() {
            eprintln!("could not close recovered database: {error}");
            process::exit(5);
        }
        return;
    }
    if mode.as_deref().is_some_and(|value| value == "--backup") {
        let Some(target) = arguments.next().map(PathBuf::from) else {
            eprintln!("backup mode requires a destination path");
            process::exit(2);
        };
        if let Err(error) = database.backup(target).and_then(|()| database.close()) {
            eprintln!("could not create backup: {error}");
            process::exit(7);
        }
        return;
    }

    let result = match mode.as_deref() {
        Some("--update-indexed") => (|| {
            let mut transaction = database.begin_transaction()?;
            transaction.update(
                "users",
                DocumentId::new(1)?,
                Document::empty().with("score", Value::Integer(2)),
            )?;
            transaction.commit()
        })(),
        Some("--bulk-update") => database
            .update_many(
                &Query::new(
                    "users",
                    Filter::Compare {
                        path: path("score"),
                        op: CompareOp::Gte,
                        value: Value::Integer(2),
                    },
                ),
                &[DocumentOperation::Increment(path("score"), Value::Integer(10))],
            )
            .map(|_| ()),
        Some("--bulk-delete") => database
            .delete_many(&Query::new(
                "users",
                Filter::Compare {
                    path: path("score"),
                    op: CompareOp::Gte,
                    value: Value::Integer(2),
                },
            ))
            .map(|_| ()),
        Some("--upsert") => database
            .upsert_one(
                &Query::new(
                    "users",
                    Filter::Compare {
                        path: path("key"),
                        op: CompareOp::Eq,
                        value: Value::String("upsert".into()),
                    },
                ),
                Document::empty()
                    .with("key", Value::String("upsert".into()))
                    .with("score", Value::Integer(4)),
                &[DocumentOperation::Set(
                    path("profile.state"),
                    Value::String("written".into()),
                )],
            )
            .map(|_| ()),
        Some("--mixed-transaction") => (|| {
            let mut transaction = database.begin_transaction()?;
            transaction.update(
                "users",
                DocumentId::new(1)?,
                Document::empty()
                    .with("key", Value::String("a".into()))
                    .with("score", Value::Integer(11)),
            )?;
            transaction.delete("users", DocumentId::new(2)?)?;
            transaction.insert(
                "users",
                Document::empty()
                    .with("key", Value::String("d".into()))
                    .with("score", Value::Integer(4)),
            )?;
            transaction.commit()
        })(),
        _ => (|| {
            let mut transaction = database.begin_transaction()?;
            transaction.create_collection("users")?;
            transaction.insert("users", Document::empty())?;
            transaction.insert("users", Document::empty())?;
            transaction.commit()
        })(),
    };
    if let Err(error) = result {
        eprintln!("could not commit transaction: {error}");
        process::exit(4);
    }
    process::exit(0);
}
