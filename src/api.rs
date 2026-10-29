// SPDX-License-Identifier: AGPL-3.0-only
//! Transport-neutral, backend-neutral public API foundation.
//!
//! This module owns only application-visible semantics. It does not expose
//! storage pages, cursors, indexes, borrowed executor rows, or file handles.
use std::collections::BTreeMap;

use crate::{
    CollectionId, Database, Document, DocumentId, DocumentOperation, Error, FieldPath,
    ProjectedRow, Projection, Query, Result, Transaction, Value,
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

/// Optional operation metadata carried by a future transport or driver.
///
/// The embedded backend exposes this type so drivers need not create their own
/// incompatible correlation envelope. It is deliberately not durable and does
/// not change write semantics: retrying an operation with the same ID can write
/// twice after an uncertain commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationContext {
    request_id: Option<RequestId>,
}

impl OperationContext {
    /// Creates an empty context.
    pub const fn new() -> Self {
        Self { request_id: None }
    }

    /// Associates this operation with a caller-visible correlation ID.
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// Returns the optional correlation ID without claiming idempotency.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
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

impl Capabilities {
    /// Returns `None` for an unknown name, distinct from known-but-disabled.
    ///
    /// Names are lowercase ASCII with underscores. Future capabilities are
    /// additive: older clients must treat `None` as unknown rather than false.
    pub fn supports(&self, name: &str) -> Option<bool> {
        match name {
            "native_queries" => Some(self.native_queries),
            "projections" => Some(self.projections),
            "array_contains" => Some(self.array_contains),
            "field_updates" => Some(self.field_updates),
            "transactions" => Some(self.transactions),
            "durable_request_deduplication" => Some(self.durable_request_deduplication),
            _ => None,
        }
    }
}

/// Version and declared capabilities returned by an engine instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiInfo {
    pub version: &'static str,
    pub capabilities: Capabilities,
}

/// Readiness of an already-open engine handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineHealth {
    Ready,
}

/// An owned, backend-neutral query result.
///
/// A `Document` result contains a complete document. A `Projection` result
/// contains only requested, present paths; an absent key means missing and a
/// `Value::Null` value means explicit null. An empty map is a valid projection.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicQueryRow {
    pub id: DocumentId,
    pub value: PublicRowValue,
}

/// The payload of a public query row.
#[derive(Debug, Clone, PartialEq)]
pub enum PublicRowValue {
    Document(Document),
    Projection(BTreeMap<FieldPath, Value>),
}

impl PublicQueryRow {
    fn document(id: DocumentId, document: Document) -> Self {
        Self {
            id,
            value: PublicRowValue::Document(document),
        }
    }

    fn projection(ProjectedRow { id, fields }: ProjectedRow) -> Self {
        Self {
            id,
            value: PublicRowValue::Projection(fields),
        }
    }

    /// Returns the complete document if this was an unprojected query row.
    pub fn document_value(&self) -> Option<&Document> {
        match &self.value {
            PublicRowValue::Document(document) => Some(document),
            PublicRowValue::Projection(_) => None,
        }
    }

    /// Returns projected fields if this was a projected query row.
    pub fn projected_fields(&self) -> Option<&BTreeMap<FieldPath, Value>> {
        match &self.value {
            PublicRowValue::Document(_) => None,
            PublicRowValue::Projection(fields) => Some(fields),
        }
    }
}

/// Transport-neutral operations implemented by an embedded engine.
/// Backup and administration deliberately remain outside this interface.
pub trait PublicEngine {
    /// Returns public-contract metadata without examining storage internals.
    fn api_info(&self) -> ApiInfo;
    /// Reports whether an already-open engine handle can serve operations.
    fn health(&self) -> EngineHealth;
    fn collection_names(&self) -> Result<Vec<String>>;
    fn create_collection(&mut self, name: &str) -> Result<CollectionId>;
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId>;
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>>;
    /// Returns complete owned documents after the native query's sort and page.
    fn query(&self, query: &Query) -> Result<Vec<PublicQueryRow>>;
    /// Returns owned projected fields after filtering, sorting, and pagination.
    fn query_projected(
        &self,
        query: &Query,
        projection: &Projection,
    ) -> Result<Vec<PublicQueryRow>>;
    fn update_one(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize>;
    fn update_many(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize>;
    /// Compatibility spelling for `update_many` in the API-foundation period.
    fn update(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize> {
        self.update_many(query, operations)
    }
    fn upsert_one(
        &mut self,
        query: &Query,
        insert_document: Document,
        operations: &[DocumentOperation],
    ) -> Result<(DocumentId, bool)>;
    fn delete_one(&mut self, query: &Query) -> Result<usize>;
    fn delete_many(&mut self, query: &Query) -> Result<usize>;
    /// Compatibility spelling for `delete_many` in the API-foundation period.
    fn delete(&mut self, query: &Query) -> Result<usize> {
        self.delete_many(query)
    }
    fn begin(&mut self) -> Result<Box<dyn PublicTransaction + '_>>;
}

/// A transaction tied to its engine handle; dropping it discards staged changes.
///
/// `commit` and `rollback` consume the transaction. Therefore a completed
/// transaction cannot be committed, rolled back, or used again through this
/// contract. Reads through `get` include its own staged writes.
pub trait PublicTransaction {
    fn create_collection(&mut self, name: &str) -> Result<CollectionId>;
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId>;
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>>;
    fn update_fields(
        &mut self,
        collection: &str,
        id: DocumentId,
        operations: &[DocumentOperation],
    ) -> Result<bool>;
    fn delete(&mut self, collection: &str, id: DocumentId) -> Result<bool>;
    fn commit(self: Box<Self>) -> Result<()>;
    fn rollback(self: Box<Self>);
}

impl PublicEngine for Database {
    fn api_info(&self) -> ApiInfo {
        ApiInfo {
            version: API_VERSION,
            capabilities: capabilities(),
        }
    }

    fn health(&self) -> EngineHealth {
        EngineHealth::Ready
    }

    fn collection_names(&self) -> Result<Vec<String>> {
        Database::collection_names(self)
    }

    fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        Database::create_collection(self, name)
    }
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId> {
        Database::insert(self, collection, document)
    }
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>> {
        Ok(Database::get(self, collection, id)?.cloned())
    }
    fn query(&self, query: &Query) -> Result<Vec<PublicQueryRow>> {
        Ok(self
            .execute(query)?
            .into_iter()
            .map(|row| PublicQueryRow::document(row.id, row.document.clone()))
            .collect())
    }
    fn query_projected(
        &self,
        query: &Query,
        projection: &Projection,
    ) -> Result<Vec<PublicQueryRow>> {
        Ok(self
            .execute_projected(query, projection)?
            .into_iter()
            .map(PublicQueryRow::projection)
            .collect())
    }
    fn update_one(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize> {
        Database::update_one(self, query, operations)
    }
    fn update_many(&mut self, query: &Query, operations: &[DocumentOperation]) -> Result<usize> {
        Database::update_many(self, query, operations)
    }
    fn upsert_one(
        &mut self,
        query: &Query,
        insert_document: Document,
        operations: &[DocumentOperation],
    ) -> Result<(DocumentId, bool)> {
        Database::upsert_one(self, query, insert_document, operations)
    }
    fn delete_one(&mut self, query: &Query) -> Result<usize> {
        Database::delete_one(self, query)
    }
    fn delete_many(&mut self, query: &Query) -> Result<usize> {
        Database::delete_many(self, query)
    }
    fn begin(&mut self) -> Result<Box<dyn PublicTransaction + '_>> {
        Ok(Box::new(self.begin_transaction()?))
    }
}

impl PublicTransaction for Transaction<'_> {
    fn create_collection(&mut self, name: &str) -> Result<CollectionId> {
        Transaction::create_collection(self, name)
    }
    fn insert(&mut self, collection: &str, document: Document) -> Result<DocumentId> {
        Transaction::insert(self, collection, document)
    }
    fn get(&self, collection: &str, id: DocumentId) -> Result<Option<Document>> {
        Ok(Transaction::get(self, collection, id)?.cloned())
    }
    fn update_fields(
        &mut self,
        collection: &str,
        id: DocumentId,
        operations: &[DocumentOperation],
    ) -> Result<bool> {
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
