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
mod projected;
pub(crate) mod spans;

/// Decode historical JSON document fields without migrating tuple metadata. Root keys remain ordinary field names; nested values preserve historical tagged values, byte-array preference and JSON normalization. The returned lease covers decoded payloads and live field entries under the supplied allowance.
pub fn decode_legacy_document_fields_budgeted(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    control.check()?;
    decode_legacy_document_text_fields_budgeted(text(bytes)?, control)
}

/// [`decode_legacy_document_fields_budgeted`] for a body its owner already holds as validated text.
pub fn decode_legacy_document_text_fields_budgeted(
    input: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    control.check()?;
    let fields = legacy::fields(input, control)?;
    control.check()?;
    Ok(fields)
}

/// Decode selected fields while preserving complete historical document validation. Canonical flat objects borrow unselected primitive tokens; other shapes retain the ordinary decoder before projection. Missing fields remain absent and duplicate requested names select one stored field.
pub fn decode_legacy_document_projection_budgeted(
    bytes: &[u8],
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    control.check()?;
    decode_legacy_document_text_projection_budgeted(text(bytes)?, fields, control)
}

/// [`decode_legacy_document_projection_budgeted`] for a body its owner already holds as validated text.
pub fn decode_legacy_document_text_projection_budgeted(
    input: &str,
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    control.check()?;
    let result = if let Some(selected) = projected::fields(input, fields, control)? {
        selected
    } else {
        let (mut decoded, memory) = legacy::normalized_fields(input, control)?.into_parts();
        decoded.retain(|name, _| fields.contains(&name.as_str()));
        Budgeted::new(decoded, memory)
    };
    control.check()?;
    Ok(result)
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
