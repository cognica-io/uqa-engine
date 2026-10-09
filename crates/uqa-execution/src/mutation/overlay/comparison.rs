//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible SQL comparisons retain frame masking and the caller's key order.

use super::{storage_error, CommandMutationOverlay, DocId, SQLError, StorageReadControl, Value};
use crate::query::exact_lookup::{matches_fields_with_control, FieldPresence};

pub(super) fn find_match(
    overlays: &[CommandMutationOverlay],
    table: &str,
    fields: &[String],
    values: &[Value],
    presence: FieldPresence,
    control: &StorageReadControl,
    catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
) -> Result<Option<DocId>, SQLError> {
    let mut found = None;
    let production = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    for (position, overlay) in overlays.iter().enumerate().rev() {
        let Some(rows) = overlay.table(table) else {
            continue;
        };
        let view = rows.rows.view();
        let mut staged = view.rows(None);
        while let Some((id, document)) = staged.next(control).map_err(storage_error)? {
            if found.is_some_and(|found| id >= found) {
                break;
            }
            if CommandMutationOverlay::stages(&overlays[position + 1..], table, id, control)? {
                continue;
            }
            if let Some(document) = document {
                if matches_fields_with_control(
                    document.fields.as_ref(),
                    fields,
                    values,
                    presence,
                    &production,
                    catalog,
                )? {
                    found = Some(id);
                    break;
                }
            }
        }
    }
    Ok(found)
}

pub(super) fn matches(
    overlays: &[CommandMutationOverlay],
    table: &str,
    fields: &[String],
    values: &[Value],
    kind: super::KeyKind,
    control: &StorageReadControl,
    catalog: Option<&dyn uqa_sql::expr::SQLValueCatalog>,
) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
    let mut found = uqa_core::memory::BudgetedVec::new(control.memory());
    let production = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    for (position, overlay) in overlays.iter().enumerate().rev() {
        let Some(rows) = overlay.table(table) else {
            continue;
        };
        let view = rows.rows.view();
        let mut staged = view.rows(None);
        while let Some((id, document)) = staged.next(control).map_err(storage_error)? {
            let Some(document) = document else {
                continue;
            };
            if CommandMutationOverlay::stages(&overlays[position + 1..], table, id, control)? {
                continue;
            }
            if matches_fields_with_control(
                kind.document(&document)?,
                fields,
                values,
                FieldPresence::MissingIsNull,
                &production,
                catalog,
            )? {
                found.push(id).map_err(super::resource_error)?;
            }
        }
    }
    Ok(found)
}
