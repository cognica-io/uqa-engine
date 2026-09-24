//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible SQL comparisons retain frame masking and the caller's key order.

use super::{resource_error, CommandMutationOverlay, DocId, SQLError, StorageReadControl, Value};
use crate::query::exact_lookup::{matches_fields_with_control, FieldPresence};

pub(super) fn find_match(
    overlays: &[CommandMutationOverlay],
    table: &str,
    fields: &[String],
    values: &[Value],
    presence: FieldPresence,
    control: &StorageReadControl,
) -> Result<Option<DocId>, SQLError> {
    let mut found = None;
    let production = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    for (position, overlay) in overlays.iter().enumerate().rev() {
        let Some(rows) = overlay.documents(table) else {
            continue;
        };
        for (&id, document) in rows {
            control.check().map_err(resource_error)?;
            if found.is_some_and(|found| id >= found) {
                break;
            }
            if overlays[position + 1..].iter().any(|overlay| {
                overlay
                    .documents(table)
                    .is_some_and(|rows| rows.contains_key(&id))
            }) {
                continue;
            }
            if let Some(document) = document {
                if matches_fields_with_control(
                    document.fields.as_ref(),
                    fields,
                    values,
                    presence,
                    &production,
                )? {
                    found = Some(id);
                    break;
                }
            }
        }
    }
    Ok(found)
}
