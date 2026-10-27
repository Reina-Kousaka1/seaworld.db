// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::BTreeMap;

/// Maximum encoded document size in SeaWorld format version 1.
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// A document value. Objects are stored in key order for deterministic encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Binary(Vec<u8>),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// Creates a floating point value only when it is finite.
    pub fn floating(value: f64) -> crate::Result<Self> {
        if !value.is_finite() {
            return Err(crate::Error::InvalidInput(
                "floating-point values must be finite".into(),
            ));
        }
        Ok(Self::Float(value))
    }

    /// Creates an object from fields, sorted by their UTF-8 key order.
    pub fn object(fields: impl IntoIterator<Item = (String, Value)>) -> Self {
        Self::Object(fields.into_iter().collect())
    }
}

/// Immutable top-level document with user fields separate from its stable ID.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    fields: BTreeMap<String, Value>,
}

impl Document {
    /// Creates a document from its fields.
    pub fn new(fields: BTreeMap<String, Value>) -> Self {
        Self { fields }
    }

    /// Creates an empty document.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns the document fields in deterministic key order.
    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }

    pub(crate) fn fields_mut_for_sql(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.fields
    }

    /// Returns the value for a top-level field, if it exists.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.get(field)
    }

    /// Returns a new document with the field replaced or added.
    pub fn with(mut self, field: impl Into<String>, value: Value) -> Self {
        self.fields.insert(field.into(), value);
        self
    }
}
