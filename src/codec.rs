use std::collections::BTreeMap;

use crate::{Document, Error, Result, Value, document::MAX_DOCUMENT_BYTES};

pub(crate) const MAX_CONTAINER_ITEMS: usize = 100_000;
pub(crate) const MAX_FIELD_NAME_BYTES: usize = 1_024;
pub(crate) const MAX_NESTING_DEPTH: usize = 64;

pub(crate) fn encode_document(document: &Document) -> Result<Vec<u8>> {
    let mut encoder = Encoder::default();
    encoder.count(document.fields().len(), "document field count")?;
    for (field, value) in document.fields() {
        validate_field_name(field).map_err(|message| Error::InvalidInput(message.into()))?;
        encoder.string(field, MAX_FIELD_NAME_BYTES, "field name")?;
        encoder.value(value, 1)?;
    }
    Ok(encoder.bytes)
}

pub(crate) fn decode_document(bytes: &[u8]) -> Result<Document> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::Integrity("document exceeds the maximum size".into()));
    }
    let mut input = Cursor::new(bytes);
    let count = input.count("document field count")?;
    let mut fields = BTreeMap::new();
    for _ in 0..count {
        let name = input.string(MAX_FIELD_NAME_BYTES, "field name")?;
        validate_field_name(&name).map_err(|message| Error::Integrity(message.into()))?;
        let value = input.value(1)?;
        if fields.insert(name.clone(), value).is_some() {
            return Err(Error::Integrity(format!(
                "duplicate document field {name:?}"
            )));
        }
    }
    input.finish("document")?;
    Ok(Document::new(fields))
}

#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn ensure(&self, additional: usize) -> Result<()> {
        if self
            .bytes
            .len()
            .checked_add(additional)
            .is_none_or(|size| size > MAX_DOCUMENT_BYTES)
        {
            return Err(Error::Limit(
                "document exceeds the maximum encoded size".into(),
            ));
        }
        Ok(())
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.ensure(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn i32(&mut self, value: i32) -> Result<()> {
        self.raw(&value.to_be_bytes())
    }

    fn i64(&mut self, value: i64) -> Result<()> {
        self.raw(&value.to_be_bytes())
    }

    fn length(&mut self, value: usize, label: &str) -> Result<()> {
        let length =
            i32::try_from(value).map_err(|_| Error::Limit(format!("{label} is too large")))?;
        self.i32(length)
    }

    fn count(&mut self, value: usize, label: &str) -> Result<()> {
        if value > MAX_CONTAINER_ITEMS {
            return Err(Error::Limit(format!(
                "{label} exceeds {MAX_CONTAINER_ITEMS}"
            )));
        }
        self.length(value, label)
    }

    fn string(&mut self, value: &str, max_bytes: usize, label: &str) -> Result<()> {
        let bytes = value.as_bytes();
        if bytes.len() > max_bytes {
            return Err(Error::Limit(format!(
                "{label} exceeds {max_bytes} UTF-8 bytes"
            )));
        }
        self.length(bytes.len(), label)?;
        self.raw(bytes)
    }

    fn value(&mut self, value: &Value, depth: usize) -> Result<()> {
        if depth > MAX_NESTING_DEPTH {
            return Err(Error::Limit(
                "document nesting exceeds the maximum depth".into(),
            ));
        }
        match value {
            Value::Null => self.raw(&[0]),
            Value::Bool(value) => self.raw(&[1, u8::from(*value)]),
            Value::Integer(value) => {
                self.raw(&[2])?;
                self.i64(*value)
            }
            Value::Float(value) => {
                if !value.is_finite() {
                    return Err(Error::InvalidInput(
                        "floating-point values must be finite".into(),
                    ));
                }
                self.raw(&[3])?;
                self.i64(value.to_bits() as i64)
            }
            Value::String(value) => {
                self.raw(&[4])?;
                self.string(value, MAX_DOCUMENT_BYTES, "string value")
            }
            Value::Binary(value) => {
                self.raw(&[5])?;
                if value.len() > MAX_DOCUMENT_BYTES {
                    return Err(Error::Limit("binary value is too large".into()));
                }
                self.length(value.len(), "binary value")?;
                self.raw(value)
            }
            Value::Array(values) => {
                self.raw(&[6])?;
                self.count(values.len(), "array length")?;
                for child in values {
                    self.value(child, depth + 1)?;
                }
                Ok(())
            }
            Value::Object(fields) => {
                self.raw(&[7])?;
                self.count(fields.len(), "object field count")?;
                for (field, child) in fields {
                    validate_field_name(field)
                        .map_err(|message| Error::InvalidInput(message.into()))?;
                    self.string(field, MAX_FIELD_NAME_BYTES, "field name")?;
                    self.value(child, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take(&mut self, length: usize, label: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| Error::Integrity(format!("{label} length overflow")))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| Error::Integrity(format!("truncated {label}")))?;
        self.position = end;
        Ok(value)
    }

    pub(crate) fn i32(&mut self, label: &str) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(4, label)?.try_into().expect("fixed cursor range"),
        ))
    }

    pub(crate) fn i64(&mut self, label: &str) -> Result<i64> {
        Ok(i64::from_be_bytes(
            self.take(8, label)?.try_into().expect("fixed cursor range"),
        ))
    }

    pub(crate) fn count(&mut self, label: &str) -> Result<usize> {
        let count = self.i32(label)?;
        if count < 0 || count as usize > MAX_CONTAINER_ITEMS {
            return Err(Error::Integrity(format!("{label} is out of bounds")));
        }
        Ok(count as usize)
    }

    pub(crate) fn string(&mut self, max_bytes: usize, label: &str) -> Result<String> {
        let length = self.i32(&format!("{label} length"))?;
        if length < 0 || length as usize > max_bytes {
            return Err(Error::Integrity(format!("{label} length is out of bounds")));
        }
        String::from_utf8(self.take(length as usize, label)?.to_vec())
            .map_err(|_| Error::Integrity(format!("{label} is not valid UTF-8")))
    }

    pub(crate) fn bytes_exact(&mut self, length: usize, label: &str) -> Result<Vec<u8>> {
        Ok(self.take(length, label)?.to_vec())
    }

    pub(crate) fn finish(&self, label: &str) -> Result<()> {
        if self.remaining() != 0 {
            return Err(Error::Integrity(format!("{label} contains trailing bytes")));
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_NESTING_DEPTH {
            return Err(Error::Integrity(
                "document nesting exceeds the maximum depth".into(),
            ));
        }
        let tag = self.take(1, "value tag")?[0];
        match tag {
            0 => Ok(Value::Null),
            1 => match self.take(1, "boolean value")?[0] {
                0 => Ok(Value::Bool(false)),
                1 => Ok(Value::Bool(true)),
                _ => Err(Error::Integrity("boolean value must be 0 or 1".into())),
            },
            2 => Ok(Value::Integer(self.i64("integer value")?)),
            3 => {
                let value = f64::from_bits(self.i64("floating-point value")? as u64);
                if !value.is_finite() {
                    return Err(Error::Integrity(
                        "floating-point value must be finite".into(),
                    ));
                }
                Ok(Value::Float(value))
            }
            4 => Ok(Value::String(
                self.string(MAX_DOCUMENT_BYTES, "string value")?,
            )),
            5 => {
                let length = self.i32("binary value length")?;
                if length < 0 || length as usize > MAX_DOCUMENT_BYTES {
                    return Err(Error::Integrity(
                        "binary value length is out of bounds".into(),
                    ));
                }
                Ok(Value::Binary(
                    self.take(length as usize, "binary value")?.to_vec(),
                ))
            }
            6 => {
                let count = self.count("array length")?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(values))
            }
            7 => {
                let count = self.count("object field count")?;
                let mut fields = BTreeMap::new();
                for _ in 0..count {
                    let name = self.string(MAX_FIELD_NAME_BYTES, "field name")?;
                    validate_field_name(&name)
                        .map_err(|message| Error::Integrity(message.into()))?;
                    let value = self.value(depth + 1)?;
                    if fields.insert(name.clone(), value).is_some() {
                        return Err(Error::Integrity(format!("duplicate object field {name:?}")));
                    }
                }
                Ok(Value::Object(fields))
            }
            _ => Err(Error::Integrity(format!(
                "unknown document value tag {tag}"
            ))),
        }
    }
}

fn validate_field_name(field: &str) -> std::result::Result<(), &'static str> {
    if field.is_empty() || field.contains('\0') {
        return Err("field name is empty or contains NUL");
    }
    if field.len() > MAX_FIELD_NAME_BYTES {
        return Err("field name exceeds the maximum UTF-8 size");
    }
    Ok(())
}
