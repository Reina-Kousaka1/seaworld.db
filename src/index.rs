// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use crate::codec::Cursor;
use crate::{Document, DocumentId, Error, Result, Value};

pub const MAX_INDEXES: usize = 1_024;
pub const MAX_FIELD_PATH_DEPTH: usize = 64;
pub(crate) const MAX_INDEX_KEY_BYTES: usize = 1_024;

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
    pub(crate) root_page_id: u64,
    pub(crate) root_generation: u64,
    pub(crate) entries: BTreeMap<IndexValue, BTreeSet<DocumentId>>,
}

impl SecondaryIndex {
    pub(crate) fn new(id: IndexId, collection_id: i64, path: FieldPath, unique: bool) -> Self {
        Self {
            id,
            collection_id,
            path,
            unique,
            root_page_id: 0,
            root_generation: 0,
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

    pub(crate) fn encoded_len(&self) -> Result<usize> {
        match self {
            Self::Null => Ok(1),
            Self::Bool(_) => Ok(2),
            Self::Integer(_) | Self::Float(_) => Ok(9),
            Self::String(value) if value.len() <= MAX_INDEX_KEY_BYTES => Ok(5 + value.len()),
            Self::String(_) => Err(Error::Limit(format!(
                "indexed string key exceeds {MAX_INDEX_KEY_BYTES} bytes"
            ))),
        }
    }

    pub(crate) fn encode(&self, output: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Null => output.push(0),
            Self::Bool(value) => output.extend_from_slice(&[1, u8::from(*value)]),
            Self::Integer(value) => {
                output.push(2);
                output.extend_from_slice(&value.to_be_bytes());
            }
            Self::Float(value) => {
                output.push(3);
                output.extend_from_slice(&value.0.to_bits().to_be_bytes());
            }
            Self::String(value) => {
                self.encoded_len()?;
                let length = i32::try_from(value.len())
                    .map_err(|_| Error::Limit("index key length overflow".into()))?;
                output.push(4);
                output.extend_from_slice(&length.to_be_bytes());
                output.extend_from_slice(value.as_bytes());
            }
        }
        Ok(())
    }

    pub(crate) fn decode(input: &mut Cursor<'_>) -> Result<Self> {
        match input.u8("index key type")? {
            0 => Ok(Self::Null),
            1 => match input.u8("index boolean key")? {
                0 => Ok(Self::Bool(false)),
                1 => Ok(Self::Bool(true)),
                _ => Err(Error::Integrity("index boolean key is invalid".into())),
            },
            2 => Ok(Self::Integer(input.i64("index integer key")?)),
            3 => {
                let value = f64::from_bits(input.i64("index float key")? as u64);
                if !value.is_finite() {
                    return Err(Error::Integrity("index float key is not finite".into()));
                }
                Ok(Self::Float(FloatKey(value)))
            }
            4 => Ok(Self::String(
                input.string(MAX_INDEX_KEY_BYTES, "index string key")?,
            )),
            value => Err(Error::Integrity(format!("unknown index key type {value}"))),
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
