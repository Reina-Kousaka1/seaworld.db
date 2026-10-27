// SPDX-License-Identifier: AGPL-3.0-only
use std::{fmt, io};

/// Errors returned by SeaWorld's parser, storage, and database API.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Randomness(getrandom::Error),
    Locked,
    ReadOnly,
    RecoveryRequired,
    ActiveSnapshots,
    LockPoisoned,
    Format(String),
    Integrity(String),
    InvalidInput(String),
    Limit(String),
    NotFound(String),
    Duplicate(String),
    UpgradeRequired(String),
    ConstraintViolation(String),
    UnsupportedIndexValue(String),
    TransactionState(String),
    Query(String),
}

/// Result type used by the SeaWorld DB API.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Randomness(error) => write!(formatter, "could not generate database ID: {error}"),
            Self::Locked => formatter.write_str("database is already open for writing"),
            Self::ReadOnly => formatter.write_str("database handle is read-only"),
            Self::RecoveryRequired => {
                formatter.write_str("database requires a writable open for recovery")
            }
            Self::ActiveSnapshots => {
                formatter.write_str("operation is unavailable while snapshots are active")
            }
            Self::LockPoisoned => formatter.write_str("database snapshot state is unavailable"),
            Self::Format(message) => write!(formatter, "invalid database format: {message}"),
            Self::Integrity(message) => write!(formatter, "database integrity error: {message}"),
            Self::InvalidInput(message) => write!(formatter, "invalid input: {message}"),
            Self::Limit(message) => write!(formatter, "database limit exceeded: {message}"),
            Self::NotFound(message) => write!(formatter, "not found: {message}"),
            Self::Duplicate(message) => write!(formatter, "duplicate: {message}"),
            Self::UpgradeRequired(message) => write!(formatter, "upgrade required: {message}"),
            Self::ConstraintViolation(message) => {
                write!(formatter, "constraint violation: {message}")
            }
            Self::UnsupportedIndexValue(message) => {
                write!(formatter, "unsupported index value: {message}")
            }
            Self::TransactionState(message) => write!(formatter, "transaction state: {message}"),
            Self::Query(message) => write!(formatter, "query error: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Randomness(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
