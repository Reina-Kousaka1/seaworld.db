use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use crate::{Document, DocumentId, Error, Result, Value};

pub const MAX_INDEXES: usize = 1_024;
pub const MAX_FIELD_PATH_DEPTH: usize = 64;

/// Stable database-local identifier for an index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IndexId(i64);

impl IndexId {
    /// Returns the positive identifier value.
    pub const fn value(self) -> i64 {
        self.0
    }

    pub(crate) const fn from_positive(value: i64) -> Self {
        Self(value)
    }
}

/// A validated path through nested document objects.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// Parses a dotted object path such as `profile.username`.
    pub fn parse(path: &str) -> Result<Self> {
        Self::new(path.split('.').map(str::to_owned))
    }

    /// Builds a path from explicit segments, allowing literal dots inside a key.
    pub fn new(segments: impl IntoIterator<Item = String>) -> Result<Self> {
        let segments: Vec<_> = segments.into_iter().collect();
        if segments.is_empty() || segments.len() > MAX_FIELD_PATH_DEPTH {
            return Err(Error::Query(format!(
                "field path depth must be between 1 and {MAX_FIELD_PATH_DEPTH}"
            )));
        }
        for segment in &segments {
            if segment.is_empty() || segment.contains('\0') || segment.len() > 1_024 {
                return Err(Error::Query(
                    "field path segments must be non-empty, NUL-free and at most 1,024 UTF-8 bytes"
                        .into(),
                ));
            }
        }
        Ok(Self(segments))
    }

    /// Returns the path's object-key segments.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Resolves this path through objects only; arrays are not traversed.
    pub fn get<'a>(&self, document: &'a Document) -> Option<&'a Value> {
        let (first, rest) = self.0.split_first()?;
        let mut value = document.get(first)?;
        for segment in rest {
            value = match value {
                Value::Object(fields) => fields.get(segment)?,
                _ => return None,
            };
        }
        Some(value)
    }
}

impl std::fmt::Display for FieldPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str(".")?;
            }
            f.write_str(segment)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SecondaryIndex {
    pub(crate) id: IndexId,
    pub(crate) collection_id: i64,
    pub(crate) path: FieldPath,
    pub(crate) unique: bool,
    pub(crate) entries: BTreeMap<IndexValue, BTreeSet<DocumentId>>,
}

impl SecondaryIndex {
    pub(crate) fn new(id: IndexId, collection_id: i64, path: FieldPath, unique: bool) -> Self {
        Self {
            id,
            collection_id,
            path,
            unique,
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn key_for(&self, document: &Document) -> Result<Option<IndexValue>> {
        self.path
            .get(document)
            .map(IndexValue::from_value)
            .transpose()
    }

    pub(crate) fn insert(&mut self, id: DocumentId, document: &Document) -> Result<()> {
        let Some(key) = self.key_for(document)? else {
            return Ok(());
        };
        if self.unique
            && self
                .entries
                .get(&key)
                .is_some_and(|ids| ids.iter().any(|existing| *existing != id))
        {
            return Err(Error::ConstraintViolation(format!(
                "unique index {} already contains value at document {id:?}",
                self.path
            )));
        }
        self.entries.entry(key).or_default().insert(id);
        Ok(())
    }

    pub(crate) fn remove_key(&mut self, id: DocumentId, key: &IndexValue) {
        let became_empty = self.entries.get_mut(key).is_some_and(|ids| {
            ids.remove(&id);
            ids.is_empty()
        });
        if became_empty {
            self.entries.remove(key);
        }
    }

    pub(crate) fn insert_key(&mut self, id: DocumentId, key: IndexValue) {
        self.entries.entry(key).or_default().insert(id);
    }
}

#[derive(Debug, Clone)]
pub(crate) enum IndexValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(FloatKey),
    String(String),
}

impl IndexValue {
    pub(crate) fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(Self::Null),
            Value::Bool(value) => Ok(Self::Bool(*value)),
            Value::Integer(value) => Ok(Self::Integer(*value)),
            Value::Float(value) if value.is_finite() => Ok(Self::Float(FloatKey(*value))),
            Value::String(value) => Ok(Self::String(value.clone())),
            Value::Float(_) => Err(Error::UnsupportedIndexValue(
                "non-finite floating-point value".into(),
            )),
            Value::Binary(_) => Err(Error::UnsupportedIndexValue("binary".into())),
            Value::Array(_) => Err(Error::UnsupportedIndexValue("array".into())),
            Value::Object(_) => Err(Error::UnsupportedIndexValue("object".into())),
        }
    }
}

impl PartialEq for IndexValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for IndexValue {}

impl PartialOrd for IndexValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexValue {
    fn cmp(&self, other: &Self) -> Ordering {
        use IndexValue::{Bool, Float, Integer, Null, String};
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Null, _) => Ordering::Less,
            (_, Null) => Ordering::Greater,
            (Bool(left), Bool(right)) => left.cmp(right),
            (Bool(_), _) => Ordering::Less,
            (_, Bool(_)) => Ordering::Greater,
            (Integer(left), Integer(right)) => left.cmp(right),
            (Integer(_), _) => Ordering::Less,
            (_, Integer(_)) => Ordering::Greater,
            (Float(left), Float(right)) => left.cmp(right),
            (Float(_), _) => Ordering::Less,
            (_, Float(_)) => Ordering::Greater,
            (String(left), String(right)) => left.cmp(right),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FloatKey(f64);

impl PartialEq for FloatKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for FloatKey {}

impl PartialOrd for FloatKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FloatKey {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.0 == other.0 {
            Ordering::Equal
        } else {
            self.0.total_cmp(&other.0)
        }
    }
}
