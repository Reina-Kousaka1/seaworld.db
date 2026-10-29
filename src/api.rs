// SPDX-License-Identifier: AGPL-3.0-only
use crate::{Error, Result};

/// Version of the transport-neutral SeaWorld API foundation, not the file format.
pub const API_VERSION: &str = "0.1";

/// An application-supplied correlation ID. It does not provide deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Validates a nonempty, bounded, printable ASCII correlation identifier.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
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
