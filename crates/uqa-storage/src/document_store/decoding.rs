//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared historical document value semantics for native and Key/Value provider codecs.

use super::Document;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{json::JsonReadError, memory::Budgeted, JsonValueDecoder, Value};

mod legacy;
pub(crate) mod spans;

/// Decode historical JSON document fields without migrating tuple metadata. Root keys remain ordinary field names; nested values preserve historical tagged values, byte-array preference and JSON normalization. The returned lease covers decoded payloads and live field entries under the supplied allowance.
pub fn decode_legacy_document_fields_budgeted(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    control.check()?;
    let fields = legacy::fields(text(bytes)?, control)?;
    control.check()?;
    Ok(fields)
}

/// Decode one historical field value with the same normalization and tagged-value rules as a complete legacy document.
pub fn decode_legacy_json_value_budgeted(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    control.check()?;
    let value = legacy::single_value(text(bytes)?, control)?;
    control.check()?;
    Ok(value)
}

pub(crate) fn decoder(control: &StorageReadControl) -> JsonValueDecoder<'_> {
    JsonValueDecoder::new(control.memory(), control.cancellation())
}

pub(crate) fn text(bytes: &[u8]) -> StorageBackendResult<&str> {
    std::str::from_utf8(bytes).map_err(|_| invalid_json("document is not valid UTF-8"))
}

pub(crate) fn invalid_json(message: &'static str) -> StorageBackendError {
    StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(message))
}

pub(crate) fn read_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::InvalidJson => invalid_json("invalid persisted document JSON"),
        JsonReadError::Memory(error) => StorageBackendError::Memory(error),
        JsonReadError::Cancelled(error) => StorageBackendError::Cancelled(error),
    }
}

fn other_error(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
