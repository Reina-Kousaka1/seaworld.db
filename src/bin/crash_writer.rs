use std::{env, path::PathBuf, process};

use seaworld_db::{Database, Document};

fn main() {
    let mut arguments = env::args_os().skip(1);
    let Some(path) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: crash_writer <database-path> [--recover]");
        process::exit(2);
    };
    let recovery_mode = arguments.next().is_some_and(|value| value == "--recover");
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
