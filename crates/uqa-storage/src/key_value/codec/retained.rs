//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled document decoding preserves each durable format's value semantics and transfers its field lease to immutable readers.

use uqa_core::{
    json::{JsonReader, JsonToken},
    memory::Budgeted,
};

use super::{
    migrate_legacy_stored_document, Document, DocumentMetadata, StorageBackendResult,
    DOCUMENT_VALUE_V1_PREFIX, DOCUMENT_VALUE_V2_PREFIX,
};
use crate::{
    document_store::{
        decoding::{
            decode_legacy_document_fields_budgeted, decoder, invalid_json, read_error,
            spans::ContainerSpans, text,
        },
        RetainedDocumentFields, RetainedStoredDocument,
    },
    read_control::StorageReadControl,
};

pub(in crate::key_value) fn decode_retained_stored_document_value(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedStoredDocument> {
    control.check()?;
    let (fields, metadata) = if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V2_PREFIX) {
        current_fields(body, control)?
    } else {
        let fields = if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V1_PREFIX) {
            decoder(control).fields(text(body)?).map_err(read_error)?
        } else {
            decode_legacy_document_fields_budgeted(bytes, control)?
        };
        migrate(fields)?
    };
    let fields = RetainedDocumentFields::from_budgeted(fields, control)?;
    control.check()?;
    Ok(RetainedStoredDocument::with_metadata(fields, metadata))
}

fn current_fields(
    input: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<(Budgeted<Document>, DocumentMetadata)> {
    let spans = ContainerSpans::read_envelope(input, control)?;
    let (fields, xmin) = match &spans {
        ContainerSpans::Object(members) => {
            let mut fields = None;
            let mut xmin = None;
            for member in members.iter() {
                control.check()?;
                match member.name.as_str() {
                    "fields" => {
                        if fields.replace(member.value).is_some() {
                            return Err(invalid_json("duplicate document fields"));
                        }
                    }
                    "tuple_xmin" => {
                        if xmin.replace(member.value).is_some() {
                            return Err(invalid_json("duplicate document tuple_xmin"));
                        }
                    }
                    // Unknown envelope fields are structurally validated by the shared reader without choosing a numeric or tagged-value representation.
                    _ => {}
                }
            }
            (
                fields.ok_or_else(|| invalid_json("missing document fields"))?,
                xmin,
            )
        }
        ContainerSpans::Array(values) if values.len() == 2 => (values[0], Some(values[1])),
        ContainerSpans::Array(_) => {
            return Err(invalid_json("document record needs two sequence elements"));
        }
    };
    let tuple_xmin = xmin
        .map(|value| decode_xmin(value, control))
        .transpose()?
        .flatten();
    let fields = decoder(control)
        .with_depth_limit(126)
        .fields(text(fields)?)
        .map_err(read_error)?;
    let metadata =
        tuple_xmin.map_or_else(DocumentMetadata::default, DocumentMetadata::with_tuple_xmin);
    Ok((fields, metadata))
}

fn decode_xmin(input: &[u8], control: &StorageReadControl) -> StorageBackendResult<Option<u32>> {
    let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation());
    let event = reader
        .next_event()
        .map_err(read_error)?
        .ok_or_else(|| invalid_json("missing tuple_xmin value"))?;
    let value = match event.token {
        JsonToken::Null => None,
        JsonToken::Number(number) => Some(
            number
                .parse::<u32>()
                .map_err(|_| invalid_json("tuple_xmin is outside the u32 range"))?,
        ),
        _ => return Err(invalid_json("tuple_xmin is not an integer or null")),
    };
    if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid_json("trailing tuple_xmin value"));
    }
    Ok(value)
}

fn migrate(
    fields: Budgeted<Document>,
) -> StorageBackendResult<(Budgeted<Document>, DocumentMetadata)> {
    let (fields, memory) = fields.into_parts();
    let (fields, metadata) = migrate_legacy_stored_document(fields, true)?.into_parts();
    Ok((Budgeted::new(fields, memory), metadata))
}

#[cfg(test)]
mod tests;
