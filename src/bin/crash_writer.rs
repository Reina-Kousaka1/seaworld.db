// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{env, path::PathBuf, process};

use seaworld_db::{Database, Document, DocumentId, Value};

fn main() {
    let mut arguments = env::args_os().skip(1);
    let Some(path) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: crash_writer <database-path> [--recover]");
        process::exit(2);
    };
    let mode = arguments.next();
    let recovery_mode = mode.as_deref().is_some_and(|value| value == "--recover");
    let mut database = match Database::open(path) {
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
    if mode
        .as_deref()
        .is_some_and(|value| value == "--update-indexed")
    {
        let result = (|| {
            let mut transaction = database.begin_transaction()?;
            transaction.update(
                "users",
                DocumentId::new(1)?,
                Document::empty().with("score", Value::Integer(2)),
            )?;
            transaction.commit()
        })();
        if let Err(error) = result {
            eprintln!("could not update indexed document: {error}");
            process::exit(6);
        }
        process::exit(0);
    }
    let result = (|| {
        let mut transaction = database.begin_transaction()?;
        transaction.create_collection("users")?;
        transaction.insert("users", Document::empty())?;
        transaction.insert("users", Document::empty())?;
        transaction.commit()
    })();
    if let Err(error) = result {
        eprintln!("could not commit transaction: {error}");
        process::exit(4);
    }
    process::exit(0);
}
