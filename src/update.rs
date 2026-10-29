// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::BTreeMap;

use crate::{Document, Error, FieldPath, Result, Value};

/// A single document mutation. Operations are applied in order.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentOperation {
    Set(FieldPath, Value),
    Unset(FieldPath),
    Increment(FieldPath, Value),
    Decrement(FieldPath, Value),
}

/// Applies bounded operations to a copy, leaving the input unchanged on error.
pub fn apply_document_operations(
    document: &Document,
    operations: &[DocumentOperation],
) -> Result<Document> {
    if operations.len() > 1_024 {
        return Err(Error::Limit("too many document operations".into()));
    }
    let mut result = document.clone();
    for operation in operations {
        let (path, create) = match operation {
            DocumentOperation::Set(path, _)
            | DocumentOperation::Increment(path, _)
            | DocumentOperation::Decrement(path, _) => (path, true),
            DocumentOperation::Unset(path) => (path, false),
        };
        let (last, parents) = path.segments().split_last().ok_or_else(|| {
            Error::InvalidInput("document update requires a nonempty path".into())
        })?;
        let mut fields = result.fields_mut_for_sql();
        let mut missing = false;
        for segment in parents {
            if create {
                fields
                    .entry(segment.clone())
                    .or_insert_with(|| Value::Object(BTreeMap::new()));
            }
            match fields.get_mut(segment) {
                Some(Value::Object(object)) => fields = object,
                None => {
                    missing = true;
                    break;
                }
                Some(_) => {
                    return Err(Error::InvalidInput(
                        "update path traverses a non-object".into(),
                    ));
                }
            }
        }
        if missing {
            continue;
        }
        match operation {
            DocumentOperation::Set(_, value) => {
                fields.insert(last.clone(), value.clone());
            }
            DocumentOperation::Unset(_) => {
                fields.remove(last);
            }
            DocumentOperation::Increment(_, amount) | DocumentOperation::Decrement(_, amount) => {
                let subtract = matches!(operation, DocumentOperation::Decrement(..));
                let current = fields.get(last);
                let value =
                    match (current, amount) {
                        (None, Value::Integer(amount)) => {
                            let value = if subtract {
                                0_i64.checked_sub(*amount)
                            } else {
                                Some(*amount)
                            };
                            Value::Integer(value.ok_or_else(|| {
                                Error::InvalidInput("integer update overflow".into())
                            })?)
                        }
                        (Some(Value::Integer(current)), Value::Integer(amount)) => {
                            let value = if subtract {
                                current.checked_sub(*amount)
                            } else {
                                current.checked_add(*amount)
                            };
                            Value::Integer(value.ok_or_else(|| {
                                Error::InvalidInput("integer update overflow".into())
                            })?)
                        }
                        (current, Value::Float(amount)) if amount.is_finite() => {
                            let base = match current {
                                None => 0.0,
                                Some(Value::Float(value)) => *value,
                                _ => {
                                    return Err(Error::InvalidInput(
                                        "numeric update type mismatch".into(),
                                    ));
                                }
                            };
                            Value::floating(if subtract {
                                base - amount
                            } else {
                                base + amount
                            })?
                        }
                        _ => {
                            return Err(Error::InvalidInput(
                                "numeric update requires matching numeric types".into(),
                            ));
                        }
                    };
                fields.insert(last.clone(), value);
            }
        }
    }
    crate::codec::encode_document(&result)?;
    Ok(result)
}
