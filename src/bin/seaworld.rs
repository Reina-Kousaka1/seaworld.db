// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
use std::{
    env,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use seaworld_db::{Database, Error, SqlResult};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("seaworld: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let Some(first) = args.next() else {
        print_usage();
        return Ok(());
    };
    let command = first.to_string_lossy().into_owned();
    match command.as_str() {
        "--help" | "-h" => {
            ensure_no_extra(args)?;
            print_usage();
            Ok(())
        }
        "check" | "verify" => {
            let path = required_path(args.next(), "check or verify requires a database path")?;
            ensure_no_extra(args)?;
            let database = Database::open_reader(&path)?;
            print_integrity(&database)?;
            database.close()?;
            Ok(())
        }
        "backup" => {
            let source = required_path(args.next(), "backup requires a source path")?;
            let target = required_path(args.next(), "backup requires a new destination path")?;
            ensure_no_extra(args)?;
            let database = Database::open_reader(&source)?;
            database.backup(&target)?;
            database.close()?;
            println!("Backup written to {}", target.display());
            Ok(())
        }
        "restore" => {
            let source = required_path(args.next(), "restore requires a source path")?;
            let target = required_path(args.next(), "restore requires a new destination path")?;
            ensure_no_extra(args)?;
            Database::restore_backup(&source, &target)?;
            println!("Restored copy written to {}", target.display());
            Ok(())
        }
        _ => {
            let path = PathBuf::from(first);
            if args.next().is_some() {
                return Err("expected one database path".into());
            }
            shell(path)
        }
    }
}

fn shell(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut database = open_or_create(&path)?;
    println!("SeaWorld DB 0.0.1-dev (experimental)");
    println!("Type .help for commands; SQL support is a documented subset.");
    let stdin = io::stdin();
    let mut input = String::new();
    loop {
        print!("seaworld> ");
        io::stdout().flush()?;
        input.clear();
        if stdin.lock().read_line(&mut input)? == 0 {
            break;
        }
        let line = input.trim();
        if line.is_empty() {
            continue;
        }
        if line == ".quit" || line == ".exit" {
            break;
        }
        match line {
            ".help" => print_help(),
            ".collections" => {
                for name in database.collection_names()? {
                    println!("{name}");
                }
            }
            ".indexes" => print_indexes(&database)?,
            ".info" => print_integrity(&database)?,
            ".check" => {
                let reader = Database::open_reader(&path)?;
                print_integrity(&reader)?;
                reader.close()?;
            }
            ".schema" => print_schema(&database)?,
            _ if line.starts_with(".open ") => {
                let requested = unquote_path(line[6..].trim());
                if requested.is_empty() {
                    println!("usage: .open PATH");
                    continue;
                }
                let candidate = PathBuf::from(requested);
                match open_or_create(&candidate) {
                    Ok(replacement) => {
                        database.close()?;
                        database = replacement;
                        println!("Opened {}", candidate.display());
                    }
                    Err(error) => println!("error: {error}"),
                }
            }
            _ => match database.execute_sql(line, &[]) {
                Ok(result) => print_sql_result(result),
                Err(error) => println!("error: {error}"),
            },
        }
    }
    database.close()?;
    Ok(())
}

fn open_or_create(path: &Path) -> Result<Database, Error> {
    if path.exists() {
        Database::open(path)
    } else {
        Database::create(path)
    }
}

fn required_path(
    value: Option<std::ffi::OsString>,
    message: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, message).into())
}

fn ensure_no_extra(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    if arguments.next().is_some() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many arguments").into());
    }
    Ok(())
}

fn print_integrity(database: &Database) -> Result<(), Error> {
    let report = database.check_integrity()?;
    println!("Integrity check passed");
    println!("Format Version: {}", report.format_version);
    println!("Database ID: {}", hex(&report.database_id));
    println!("File Size: {} bytes", report.file_bytes);
    println!("Collections: {}", report.collection_count);
    println!("Documents: {}", report.document_count);
    println!("Indexes: {}", report.index_count);
    println!("Persistent Pages: {}", report.persistent_page_count);
    println!("MVCC Watermark: {}", report.committed_version);
    println!(
        "Recovery Discarded Tail: {} bytes",
        report.discarded_tail_bytes
    );
    Ok(())
}

fn print_indexes(database: &Database) -> Result<(), Error> {
    for index in database.indexes()? {
        println!(
            "{}: {}.{} unique={} root-generation={}",
            index.id.value(),
            index.collection,
            index.path,
            index.unique,
            index
                .root_generation
                .map_or_else(|| "none".to_owned(), |generation| generation.to_string())
        );
    }
    Ok(())
}

fn print_schema(database: &Database) -> Result<(), Error> {
    println!("Document-native flexible schema");
    for collection in database.collection_names()? {
        println!("COLLECTION {collection}");
        for index in database
            .indexes()?
            .into_iter()
            .filter(|index| index.collection == collection)
        {
            println!("  INDEX {} unique={}", index.path, index.unique);
        }
    }
    Ok(())
}

fn print_sql_result(result: SqlResult) {
    match result {
        SqlResult::Rows(rows) => {
            let count = rows.len();
            for row in rows {
                println!("{} {:?}", row.id.value(), row.values);
            }
            println!("{count} row(s)");
        }
        SqlResult::AffectedRows(count) => println!("{count} row(s) affected"),
        SqlResult::CollectionCreated(id) => println!("Collection created ({})", id.value()),
        SqlResult::Plan(plan) => println!("{plan:?}"),
    }
}

fn unquote_path(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
        .or_else(|| {
            path.strip_prefix('\'')
                .and_then(|path| path.strip_suffix('\''))
        })
        .unwrap_or(path)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn print_usage() {
    println!("SeaWorld DB experimental embedded database");
    println!("Usage:");
    println!("  seaworld DATABASE.seaworld.db");
    println!("  seaworld check DATABASE.seaworld.db");
    println!("  seaworld backup SOURCE.seaworld.db NEW_BACKUP.seaworld.db");
    println!("  seaworld verify BACKUP.seaworld.db");
    println!("  seaworld restore BACKUP.seaworld.db NEW_DATABASE.seaworld.db");
}

fn print_help() {
    println!(".help         show commands");
    println!(".open PATH    open or create another database");
    println!(".collections  list collections");
    println!(".indexes      list index definitions");
    println!(".schema       show flexible collections and index paths");
    println!(".info         show format, counts, and recovery metadata");
    println!(".check        run the read-only integrity check");
    println!(".quit         close the database and exit");
    println!("SQL subset    SELECT/INSERT/UPDATE/DELETE/CREATE COLLECTION/EXPLAIN");
}
