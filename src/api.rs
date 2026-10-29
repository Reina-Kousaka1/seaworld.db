// SPDX-License-Identifier: AGPL-3.0-only
use crate::{
    CollectionId, Database, Document, DocumentId, DocumentOperation, Error, Query, Result,
    SnapshotRow, Transaction,
};

/// Version of the transport-neutral SeaWorld API foundation, not the file format.
pub const API_VERSION: &str = "0.1";

/// An application-supplied correlation ID. It does not provide deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Validates a nonempty, bounded, printable ASCII correlation identifier.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(Error::InvalidInput("invalid request ID".into()));
        }
        Ok(Self(value))
    }

    /// Returns the caller-supplied identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Implemented engine capabilities. No network service or idempotency is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub native_queries: bool,
    pub projections: bool,
    pub array_contains: bool,
    pub field_updates: bool,
    pub transactions: bool,
    pub durable_request_deduplication: bool,
}

/// Returns the capabilities of the existing embedded engine.
pub const fn capabilities() -> Capabilities {
    Capabilities {
        native_queries: true,
        projections: true,
        array_contains: true,
        field_updates: true,
        transactions: true,
        durable_request_deduplication: false,
    }
}

/// Transport-neutral operations implemented by an embedded engine.
/// Backup and administration deliberately remain outside this interface.
pub trait PublicEngine {
    fn create_collection(&mut self, name: &str) -> Result<CollectionId>;
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId>;
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>>;
    fn query(&self, query: &Query) -> Result<Vec<SnapshotRow>>;
    fn update(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize>;
    fn delete(&mut self, query: &Query) -> Result<usize>;
    fn begin(&mut self) -> Result<Box<dyn PublicTransaction + '_>>;
}

/// A transaction tied to its engine handle; dropping it discards staged changes.
pub trait PublicTransaction {
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId>;
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>>;
    fn update_fields(&mut self, collection: &str, id: DocumentId, operations: &[DocumentOperation]) -> Result<bool>;
    fn delete(&mut self, collection: &str, id: DocumentId) -> Result<bool>;
    fn commit(self: Box<Self>) -> Result<()>;
    fn rollback(self: Box<Self>);
}

impl PublicEngine for Database {
    fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        Database::create_collection(self, name)
    }
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId> {
        Database::insert(self, collection, document)
    }
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>> {
        Ok(Database::get(self, collection, id)?.cloned())
    }
    fn query(&self, query: &Query) -> Result<Vec<SnapshotRow>> {
        Ok(self.execute(query)?.into_iter().map(|row| SnapshotRow { id: row.id, document: row.document.clone() }).collect())
    }
    fn update(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize> {
        self.update_many(query, operations)
    }
    fn delete(&mut self, query: &Query) -> Result<usize> {
        self.delete_many(query)
    }
    fn begin(&mut self) -> Result<Box<dyn PublicTransaction + '_>> {
        Ok(Box::new(self.begin_transaction()?))
    }
}

impl PublicTransaction for Transaction<'_> {
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId> {
        Transaction::insert(self, collection, document)
    }
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>> {
        Ok(Transaction::get(self, collection, id)?.cloned())
    }
    fn update_fields(&mut self, collection: &str, id: DocumentId, operations: &[DocumentOperation]) -> Result<bool> {
        Transaction::update_fields(self, collection, id, operations)
    }
    fn delete(&mut self, collection: &str, id: DocumentId) -> Result<bool> {
        Transaction::delete(self, collection, id)
    }
    fn commit(self: Box<Self>) -> Result<()> {
        Transaction::commit(*self)
    }
    fn rollback(self: Box<Self>) {
        Transaction::rollback(*self)
    }
}
