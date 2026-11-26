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
        "usage" => {
            let path = required_path(args.next(), "usage requires a database path")?;
            ensure_no_extra(args)?;
            let database = Database::open_reader(&path)?;
            let usage = database.storage_usage()?;
            println!("File bytes: {}", usage.file_bytes);
            println!("Limit bytes: {}", usage.limit_bytes);
            println!("Remaining bytes: {}", usage.remaining_bytes);
            println!(
                "Estimated live bytes (estimate): {}",
                usage.estimated_live_bytes
            );
            println!(
                "Estimated reclaimable bytes (estimate): {}",
                usage.estimated_reclaimable_bytes
            );
            println!("Stored index pages: {}", usage.persistent_page_count);
            println!("Reachable index pages: {}", usage.reachable_page_count);
            println!(
                "Compaction recommended: {}",
                if usage.compaction_recommended {
                    "yes"
                } else {
                    "no"
                }
            );
            database.close()?;
            Ok(())
        }
        "compact" => {
            let source = required_path(args.next(), "compact requires a source path")?;
            let target = required_path(args.next(), "compact requires a new destination path")?;
            ensure_no_extra(args)?;
            let database = Database::open_reader(&source)?;
            let report = database.compact_verified(&target)?;
            database.close()?;
            println!(
                "Verified compacted copy written to {}",
                report.path.display()
            );
            println!(
                "Bytes: {} -> {}",
                report.source_file_bytes, report.compacted_file_bytes
            );
            println!("The source was not changed; switch to the copy explicitly.");
            Ok(())
        }
        "--open" | "--create" => {
            let mode = if command == "--open" {
                OpenMode::ExistingOnly
            } else {
                OpenMode::CreateNew
            };
            let path = required_path(args.next(), "--open or --create requires a database path")?;
            ensure_no_extra(args)?;
            shell(path, mode)
        }
        _ => {
            let path = PathBuf::from(first);
            if args.next().is_some() {
                return Err("expected one database path".into());
            }
            shell(path, OpenMode::OpenOrCreate)
        }
    }
}

/// How the shell resolves its database path.
#[derive(Clone, Copy)]
enum OpenMode {
    /// Backward-compatible default and `.open`: open, or create and announce it.
    OpenOrCreate,
    /// `--open`: never create; a missing path is an error.
    ExistingOnly,
    /// `--create` / `.create`: create a new file; never open an existing one.
    CreateNew,
}

fn shell(path: PathBuf, mode: OpenMode) -> Result<(), Box<dyn std::error::Error>> {
    let mut database = open_with_mode(&path, mode)?;
    let mut path = path;
    println!("SeaWorld DB {} (embedded)", env!("CARGO_PKG_VERSION"));
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
            _ if line.starts_with(".open ") || line.starts_with(".create ") => {
                let (mode, rest, usage) = if let Some(rest) = line.strip_prefix(".open ") {
                    (OpenMode::OpenOrCreate, rest, "usage: .open PATH")
                } else {
                    (
                        OpenMode::CreateNew,
                        &line[".create ".len()..],
                        "usage: .create PATH",
                    )
                };
                let requested = unquote_path(rest.trim());
                if requested.is_empty() {
                    println!("{usage}");
                    continue;
                }
                let candidate = PathBuf::from(requested);
                match open_with_mode(&candidate, mode) {
                    Ok(replacement) => {
                        database.close()?;
                        database = replacement;
                        println!("Opened {}", candidate.display());
                        path = candidate;
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

fn open_with_mode(path: &Path, mode: OpenMode) -> Result<Database, Box<dyn std::error::Error>> {
    match mode {
        OpenMode::ExistingOnly => {
            if !path.exists() {
                return Err(format!(
                    "database {} does not exist; use --create to make a new one",
                    path.display()
                )
                .into());
            }
            Ok(Database::open(path)?)
        }
        OpenMode::CreateNew => Ok(Database::create(path)?),
        OpenMode::OpenOrCreate => {
            if path.exists() {
                Ok(Database::open(path)?)
            } else {
                let database: Result<Database, Error> = Database::create(path);
                let database = database?;
                eprintln!(
                    "seaworld: created new database {}; use --open to refuse creating files",
                    path.display()
                );
                Ok(database)
            }
        }
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
    println!("SeaWorld DB embedded database");
    println!("Usage:");
    println!("  seaworld DATABASE.seaworld.db          (opens, or creates and reports it)");
    println!("  seaworld --open DATABASE.seaworld.db   (existing file only)");
    println!("  seaworld --create NEW.seaworld.db      (new file only)");
    println!("  seaworld check DATABASE.seaworld.db");
    println!("  seaworld backup SOURCE.seaworld.db NEW_BACKUP.seaworld.db");
    println!("  seaworld verify BACKUP.seaworld.db");
    println!("  seaworld restore BACKUP.seaworld.db NEW_DATABASE.seaworld.db");
    println!("  seaworld usage DATABASE.seaworld.db");
    println!("  seaworld compact SOURCE.seaworld.db NEW_COMPACTED.seaworld.db");
}

fn print_help() {
    println!(".help         show commands");
    println!(".open PATH    open another database (creates it if missing, with a notice)");
    println!(".create PATH  create and open a new database");
    println!(".collections  list collections");
    println!(".indexes      list index definitions");
    println!(".schema       show flexible collections and index paths");
    println!(".info         show format, counts, and recovery metadata");
    println!(".check        run the read-only integrity check");
    println!(".quit         close the database and exit");
    println!("SQL subset    SELECT/INSERT/UPDATE/DELETE/CREATE COLLECTION/EXPLAIN");
}
