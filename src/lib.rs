//! Experimental embedded SeaWorld DB engine.
//!
//! The public API and the planned SQL adapter share one document-native engine. The
//! V1/V2 files remain readable; new databases use V3 transaction records.

mod codec;
mod document;
mod error;
mod format;
mod index;
mod mvcc;
mod query;
mod storage;

pub use document::{Document, MAX_DOCUMENT_BYTES, Value};
pub use error::{Error, Result};
pub use index::{FieldPath, IndexId, MAX_FIELD_PATH_DEPTH, MAX_INDEXES};
pub use mvcc::{
    CommitVersion, MAX_ACTIVE_SNAPSHOTS, MAX_MVCC_HISTORY_BYTES, Snapshot, SnapshotRow,
};
pub use query::{
    CompareOp, Filter, MAX_FILTER_DEPTH, MAX_FILTER_NODES, MAX_QUERY_RESULTS, Query, QueryPlan,
    QueryRow, SortDirection,
};
pub use storage::{
    Database, MAX_COLLECTIONS, MAX_DATABASE_BYTES, MAX_DOCUMENTS, RecoveryReport, Transaction,
};

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
