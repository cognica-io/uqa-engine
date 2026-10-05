//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed callbacks keep decoded payload leases after leaving the provider's fixed read scope.

use super::{DocId, KeyValueDocumentStore, StorageBackendResult, Value};
use crate::key_value::codec::{
    decode_retained_stored_document_value, document_key_prefix_controlled, other_error,
};
use crate::RetainedStoredDocument;
use uqa_core::memory::BudgetedVec;

struct Row {
    id: DocId,
    present: bool,
    document: Option<RetainedStoredDocument>,
}

impl KeyValueDocumentStore {
    pub(super) fn visit_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        // Capture every requested row on one visibility boundary, releasing physical locks before callbacks can reenter persistence.
        let (rows, control) = self.read(|view| {
            let control = view.read.control();
            control.check()?;
            if ids.is_empty() {
                return Ok((BudgetedVec::<Row>::new(control.memory()), control.clone()));
            }
            let mut selected = BudgetedVec::new(control.memory());
            selected.extend_from_slice(ids)?;
            selected.sort_unstable();
            let mut unique = 0;
            for index in 0..selected.len() {
                if unique == 0 || selected[index] != selected[unique - 1] {
                    selected[unique] = selected[index];
                    unique += 1;
                }
            }
            selected.truncate(unique);
            let prefix = document_key_prefix_controlled(view.table, control)?;
            let mut keys = selected.iter().map(|id| {
                control.check()?;
                let mut key = BudgetedVec::new(control.memory());
                key.extend_from_slice(&prefix)?;
                key.extend_from_slice(&id.to_be_bytes())?;
                Ok(key)
            });
            let mut rows = BudgetedVec::<Row>::new(control.memory());
            let mut failure = None;
            let mut capture = |key: &[u8], present: bool, value: Option<&[u8]>| {
                if failure.is_some() {
                    return Err(other_error("document point visitor has already failed"));
                }
                let result = (|| {
                    control.check()?;
                    let id = super::read::decode_id(&prefix, key)?;
                    if !key.starts_with(&prefix) || selected.get(rows.len()) != Some(&id) {
                        return Err(other_error(
                            "document point visitor returned an unexpected key",
                        ));
                    }
                    rows.reserve(1)?;
                    let document = value
                        .map(|value| decode_retained_stored_document_value(value, control))
                        .transpose()?;
                    rows.push(Row {
                        id,
                        present,
                        document,
                    })?;
                    Ok(true)
                })();
                result.map_err(|error| {
                    failure = Some(error);
                    other_error("document point visitor failed")
                })
            };
            let scanned = if fields.is_empty() {
                view.read
                    .visit_key_presence(&mut keys, &mut |key, present| capture(key, present, None))
            } else {
                view.read.visit_values(&mut keys, &mut |key, value| {
                    capture(key, value.is_some(), value)
                })
            };
            if let Some(error) = failure {
                return Err(error);
            }
            scanned?;
            control.check()?;
            if rows.len() != selected.len() {
                return Err(other_error(
                    "document point visitor did not return every requested row",
                ));
            }
            Ok((rows, control.clone()))
        })?;
        let mut projected = BudgetedVec::new(control.memory());
        projected.reserve(fields.len())?;
        for id in ids {
            control.check()?;
            let position = rows
                .binary_search_by_key(id, |row| row.id)
                .expect("captured id");
            let row = &rows[position];
            projected.clear();
            for field in fields {
                projected.push(
                    row.document
                        .as_ref()
                        .and_then(|document| document.fields().get(*field))
                        .unwrap_or(&Value::Null),
                )?;
            }
            let keep_going = visitor(*id, row.present, &projected);
            control.check()?;
            if !keep_going {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
