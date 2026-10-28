// SPDX-License-Identifier: AGPL-3.0-only
use std::cmp::Ordering;

use crate::{Document, DocumentId, Error, Result, Value, index::FieldPath};

pub const MAX_QUERY_RESULTS: usize = 100_000;
pub const MAX_FILTER_NODES: usize = 1_024;
pub const MAX_FILTER_DEPTH: usize = 64;

/// Comparison operation in the typed native query AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
}

/// Typed filter expression. It never parses or evaluates a query string.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Matches every document in the selected collection.
    All,
    IdEq(DocumentId),
    Compare {
        path: FieldPath,
        op: CompareOp,
        value: Value,
    },
    Exists {
        path: FieldPath,
        exists: bool,
    },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

impl Filter {
    /// Matches a present field equal to any supplied value, without type coercion.
    /// An empty list matches no documents. Execution validates the resulting filter.
    pub fn in_values(path: FieldPath, values: impl IntoIterator<Item = Value>) -> Result<Self> {
        let mut filters = Vec::new();
        for value in values {
            if filters.len() >= MAX_FILTER_NODES - 2 {
                return Err(Error::Limit("query filter is too complex".into()));
            }
            filters.push(Self::Compare {
                path: path.clone(),
                op: CompareOp::Eq,
                value,
            });
        }
        if filters.is_empty() {
            Ok(Self::Not(Box::new(Self::All)))
        } else {
            Ok(Self::Or(filters))
        }
    }

    /// Matches documents not matched by `in_values`, including missing fields.
    pub fn not_in_values(path: FieldPath, values: impl IntoIterator<Item = Value>) -> Result<Self> {
        Ok(Self::Not(Box::new(Self::in_values(path, values)?)))
    }
}

/// Direction for a deterministic field sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// Query specification for one collection.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub(crate) collection: String,
    pub(crate) filter: Filter,
    pub(crate) limit: usize,
    pub(crate) offset: usize,
    pub(crate) sort: Option<(FieldPath, SortDirection)>,
}

impl Query {
    /// Creates a query with the supplied filter and the default result limit.
    pub fn new(collection: impl Into<String>, filter: Filter) -> Self {
        Self {
            collection: collection.into(),
            filter,
            limit: MAX_QUERY_RESULTS,
            offset: 0,
            sort: None,
        }
    }

    /// Sets a bounded result limit. Invalid values are reported by execution.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Sets a bounded result offset. Invalid values are reported by execution.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Sorts by a validated document field path.
    pub fn sort_by(mut self, path: FieldPath, direction: SortDirection) -> Self {
        self.sort = Some((path, direction));
        self
    }

    /// Returns the collection named by this query.
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Returns the filter expression.
    pub fn filter(&self) -> &Filter {
        &self.filter
    }
}

/// A planner decision exposed by [`crate::Database::explain`].
#[derive(Debug, Clone, PartialEq)]
pub enum QueryPlan {
    PrimaryKeyLookup {
        collection: String,
        id: DocumentId,
    },
    SecondaryIndexLookup {
        collection: String,
        index_id: i64,
        path: FieldPath,
        unique: bool,
    },
    SecondaryIndexRangeLookup {
        collection: String,
        index_id: i64,
        path: FieldPath,
        operator: CompareOp,
        value: Value,
        unique: bool,
    },
    FullScan {
        collection: String,
    },
}

/// One borrowed row returned by the native query executor.
#[derive(Debug, Clone, Copy)]
pub struct QueryRow<'a> {
    pub id: DocumentId,
    pub document: &'a Document,
}

pub(crate) fn validate_filter(filter: &Filter) -> Result<()> {
    fn visit(filter: &Filter, depth: usize, nodes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > MAX_FILTER_DEPTH || *nodes > MAX_FILTER_NODES {
            return Err(Error::Limit("query filter is too complex".into()));
        }
        match filter {
            Filter::Compare { value, .. } => crate::codec::validate_value(value)?,
            Filter::And(children) | Filter::Or(children) => {
                if children.is_empty() {
                    return Err(Error::Query(
                        "AND/OR filters must contain at least one child".into(),
                    ));
                }
                for child in children {
                    visit(child, depth + 1, nodes)?;
                }
            }
            Filter::Not(child) => visit(child, depth + 1, nodes)?,
            Filter::All | Filter::IdEq(_) | Filter::Exists { .. } => {}
        }
        Ok(())
    }
    let mut nodes = 0;
    visit(filter, 1, &mut nodes)
}

pub(crate) fn matches(filter: &Filter, id: DocumentId, document: &Document) -> bool {
    match filter {
        Filter::All => true,
        Filter::IdEq(expected) => id == *expected,
        Filter::Compare { path, op, value } => match path.get(document) {
            None => matches!(op, CompareOp::Ne),
            Some(actual) => match op {
                CompareOp::Eq => equal_values(actual, value),
                CompareOp::Ne => !equal_values(actual, value),
                CompareOp::Lt => ordered_values(actual, value).is_some_and(|o| o == Ordering::Less),
                CompareOp::Lte => {
                    ordered_values(actual, value).is_some_and(|o| o != Ordering::Greater)
                }
                CompareOp::Gt => {
                    ordered_values(actual, value).is_some_and(|o| o == Ordering::Greater)
                }
                CompareOp::Gte => {
                    ordered_values(actual, value).is_some_and(|o| o != Ordering::Less)
                }
            },
        },
        Filter::Exists { path, exists } => path.get(document).is_some() == *exists,
        Filter::And(children) => children.iter().all(|child| matches(child, id, document)),
        Filter::Or(children) => children.iter().any(|child| matches(child, id, document)),
        Filter::Not(child) => !matches(child, id, document),
    }
}

pub(crate) fn equal_values(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Integer(left), Value::Integer(right)) => left == right,
        (Value::Float(left), Value::Float(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Binary(left), Value::Binary(right)) => left == right,
        (Value::Array(left), Value::Array(right)) => left == right,
        (Value::Object(left), Value::Object(right)) => left == right,
        _ => false,
    }
}

pub(crate) fn ordered_values(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        (Value::Integer(left), Value::Integer(right)) => Some(left.cmp(right)),
        (Value::Float(left), Value::Float(right)) if left.is_finite() && right.is_finite() => {
            Some(if left == right {
                Ordering::Equal
            } else {
                left.total_cmp(right)
            })
        }
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        _ => None,
    }
}
