//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend one stored-key projection at a time across resident and retained caches.

use super::{storage, ValueIndexCache};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::read_control::StorageReadControl;

/// The caller retains the selected caches throughout visitation. Errors are not a declined projection and never authorize repeating already visited rows through another source.
pub fn visit_stored_fields(
    columns: &[&ValueIndexCache],
    presence: &ValueIndexCache,
    ids: &[DocId],
    control: &StorageReadControl,
    visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
) -> Result<usize, SQLError> {
    let mut values = Vec::with_capacity(columns.len());
    let mut visited = 0;
    for &id in ids {
        control.check().map_err(storage)?;
        values.clear();
        for column in columns {
            let Some(value) = column.stored_value(id, control)? else {
                break;
            };
            values.push(value);
        }
        let exists = values.len() == columns.len()
            && (!columns.is_empty() || presence.contains(id, control)?);
        let borrowed = if exists {
            values.iter().map(std::borrow::Cow::as_ref).collect()
        } else {
            Vec::new()
        };
        visited += 1;
        if !visitor(id, exists, &borrowed) {
            break;
        }
    }
    Ok(visited)
}
