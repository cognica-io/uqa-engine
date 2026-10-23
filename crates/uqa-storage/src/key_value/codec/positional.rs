//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional projections preserve each durable codec's validation and metadata.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

use super::{
    legacy_tuple_xmin, retained::current_field_input, DOCUMENT_VALUE_V1_PREFIX,
    DOCUMENT_VALUE_V2_PREFIX, LEGACY_SYSTEM_XMIN, LEGACY_USER_XMIN_MARKER,
};
use crate::{
    document_store::decoding::{
        decoder, read_error, text, visit_legacy_fields, ProjectedDocumentFields,
        ProjectedFieldsBuilder,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};

pub(in crate::key_value) fn decode_stored_document_slots(
    bytes: &[u8],
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<ProjectedDocumentFields>> {
    control.check()?;
    let mut builder = ProjectedFieldsBuilder::new(fields, control.memory())?;
    if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V2_PREFIX) {
        // Envelope structure and tuple metadata are validated before its field
        // values, exactly as in the complete retained-document decoder.
        let (input, _) = current_field_input(body, control)?;
        decoder(control)
            .with_depth_limit(126)
            .visit_fields(text(input)?, &mut |name, value| {
                builder.push(name, value).map_err(Into::into)
            })
            .map_err(read_error)?;
    } else {
        let mut xmin = None;
        let mut collect = |name: &str, value| -> Result<(), MemoryError> {
            match name {
                LEGACY_SYSTEM_XMIN => xmin = Some(value),
                // Legacy metadata is removed from the public field namespace;
                // public xmin itself is preserved by this read contract.
                LEGACY_USER_XMIN_MARKER => {}
                _ => builder.push(name, value)?,
            }
            Ok(())
        };
        if let Some(body) = bytes.strip_prefix(DOCUMENT_VALUE_V1_PREFIX) {
            decoder(control)
                .visit_fields(text(body)?, &mut |name, value| {
                    collect(name, value).map_err(Into::into)
                })
                .map_err(read_error)?;
        } else {
            let mut selected = BudgetedVec::new(control.memory());
            selected.extend_from_slice(fields)?;
            selected.push(LEGACY_SYSTEM_XMIN)?;
            visit_legacy_fields(text(bytes)?, &selected, control, &mut |name, value| {
                collect(name, value).map_err(Into::into)
            })?;
        }
        legacy_tuple_xmin(xmin.as_deref())?;
    }
    control.check()?;
    Ok(builder.finish())
}

#[cfg(test)]
mod tests;
