// SPDX-License-Identifier: AGPL-3.0-only
use std::{fmt, io};

/// Errors returned by SeaWorld's parser, storage, and database API.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(io::Error),
    Randomness(getrandom::Error),
    Locked,
    ReadOnly,
    RecoveryRequired,
    ActiveSnapshots,
    LockPoisoned,
    Format(String),
    UnsupportedFormatVersion(u16),
    UnsupportedFeature(String),
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

/// Transport-neutral categories; details remain available on the original error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    Io,
    Locked,
    ReadOnly,
    RecoveryRequired,
    CorruptFormat,
    IntegrityFailure,
    InvalidArgument,
    MalformedDocument,
    ResourceLimit,
    NotFound,
    Conflict,
    UniqueConstraint,
    TransactionConflict,
    TransactionState,
    UnsupportedOperation,
    UnsupportedCapability,
    IncompatibleFormat,
    UnknownFormatVersion,
    BackendUnavailable,
    InternalInvariant,
}

impl ErrorCode {
    /// Stable machine-readable category; error details are not included.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Locked => "locked",
            Self::ReadOnly => "read_only",
            Self::RecoveryRequired => "recovery_required",
            Self::CorruptFormat => "corrupt_format",
            Self::IntegrityFailure => "integrity_failure",
            Self::InvalidArgument => "invalid_argument",
            Self::MalformedDocument => "malformed_document",
            Self::ResourceLimit => "resource_limit",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::UniqueConstraint => "unique_constraint",
            Self::TransactionConflict => "transaction_conflict",
            Self::TransactionState => "transaction_state",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::IncompatibleFormat => "incompatible_format",
            Self::UnknownFormatVersion => "unknown_format_version",
            Self::BackendUnavailable => "backend_unavailable",
            Self::InternalInvariant => "internal_invariant",
        }
    }
}

impl Error {
    /// Maps internal errors to a transport-neutral category without exposing paths.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Io(_) | Self::Randomness(_) => ErrorCode::Io,
            Self::Locked => ErrorCode::Locked,
            Self::ReadOnly => ErrorCode::ReadOnly,
            Self::RecoveryRequired => ErrorCode::RecoveryRequired,
            Self::Format(_) => ErrorCode::CorruptFormat,
            Self::UnsupportedFormatVersion(_) => ErrorCode::UnknownFormatVersion,
            Self::UnsupportedFeature(_) => ErrorCode::IncompatibleFormat,
            Self::Integrity(_) => ErrorCode::IntegrityFailure,
            Self::InvalidInput(_) | Self::Query(_) => ErrorCode::InvalidArgument,
            Self::Limit(_) => ErrorCode::ResourceLimit,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Duplicate(_) => ErrorCode::UniqueConstraint,
            Self::ConstraintViolation(_) => ErrorCode::Conflict,
            Self::UpgradeRequired(_) => ErrorCode::IncompatibleFormat,
            Self::UnsupportedIndexValue(_) => ErrorCode::UnsupportedOperation,
            Self::ActiveSnapshots => ErrorCode::TransactionConflict,
            Self::TransactionState(_) => ErrorCode::TransactionState,
            Self::LockPoisoned => ErrorCode::InternalInvariant,
        }
    }
}

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
            Self::UnsupportedFormatVersion(version) => {
                write!(formatter, "unsupported database format version: {version}")
            }
            Self::UnsupportedFeature(message) => {
                write!(formatter, "unsupported database feature: {message}")
            }
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
