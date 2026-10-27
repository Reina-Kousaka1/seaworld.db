use std::{env, path::PathBuf, process};

use seaworld_db::Database;

fn main() {
    let Some(path) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: crash_writer <database-path>");
        process::exit(2);
    };
    let mut database = match Database::open(path) {
        Ok(database) => database,
        Err(error) => {
            eprintln!("could not open database: {error}");
            process::exit(3);
        }
    };
    if let Err(error) = database.create_collection("users") {
        eprintln!("could not create collection: {error}");
        process::exit(4);
    }
    process::exit(0);
}
