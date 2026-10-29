// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]
//! Experimental embedded SeaWorld DB engine.
//!
//! The public API and the planned SQL adapter share one document-native engine. The
//! V1/V2/V3 files remain readable; new databases use V4 persistent index pages.

mod api;
mod btree;
mod codec;
mod document;
mod error;
mod format;
mod index;
mod mvcc;
mod query;
mod sql;
mod storage;
mod update;

pub use api::{
    API_VERSION, ApiInfo, Capabilities, EngineHealth, OperationContext, PublicEngine,
    PublicQueryRow, PublicRowValue, PublicTransaction, RequestId, capabilities,
};
pub use document::{Document, MAX_DOCUMENT_BYTES, Value};
pub use error::{Error, ErrorCode, Result};
pub use index::{FieldPath, IndexId, MAX_FIELD_PATH_DEPTH, MAX_INDEXES};
pub use mvcc::{
    CommitVersion, MAX_ACTIVE_SNAPSHOTS, MAX_MVCC_HISTORY_BYTES, Snapshot, SnapshotRow,
};
pub use query::{
    CompareOp, Filter, MAX_FILTER_DEPTH, MAX_FILTER_NODES, MAX_PROJECTED_BYTES,
    MAX_PROJECTION_FIELDS, MAX_QUERY_RESULTS, ProjectedRow, Projection, Query, QueryPlan, QueryRow,
    SortDirection,
};
pub use sql::{MAX_SQL_BYTES, MAX_SQL_TOKENS, SqlResult, SqlRow};
pub use storage::{
    Database, IndexInfo, IntegrityReport, MAX_COLLECTIONS, MAX_DATABASE_BYTES, MAX_DOCUMENTS,
    RecoveryReport, Transaction,
};
pub use update::{DocumentOperation, apply_document_operations};

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
