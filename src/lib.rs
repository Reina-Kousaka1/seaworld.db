//! Experimental embedded SeaWorld DB engine.
//!
//! The public API and the planned SQL adapter share one document-native engine. The
//! current implementation persists a versioned append-only SeaWorld v1 file.

mod codec;
mod document;
mod error;
mod format;
mod storage;

pub use document::{Document, MAX_DOCUMENT_BYTES, Value};
pub use error::{Error, Result};
pub use storage::{Database, MAX_COLLECTIONS, MAX_DATABASE_BYTES, MAX_DOCUMENTS, RecoveryReport};

/// A stable, positive identifier assigned to a document within a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentId(i64);

impl DocumentId {
    /// Creates a document identifier after checking the on-disk invariant.
    pub fn new(value: i64) -> Result<Self> {
        if value <= 0 {
            return Err(Error::InvalidInput("document IDs must be positive".into()));
        }
        Ok(Self(value))
    }

    /// Returns the signed 64-bit identifier value.
    pub const fn value(self) -> i64 {
        self.0
    }
}

/// A stable identifier for a collection within a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionId(i64);

impl CollectionId {
    /// Returns the signed 64-bit identifier value.
    pub const fn value(self) -> i64 {
        self.0
    }
}
