//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed callbacks keep decoded payload leases after leaving the provider's fixed read scope.

use super::{DocId, KeyValueDocumentStore, StorageBackendResult, Value};
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
            let mut selected = BudgetedVec::new(control.memory());
            selected.extend_from_slice(ids)?;
            selected.sort_unstable();
            let mut rows = BudgetedVec::<Row>::new(control.memory());
            for id in selected.iter().copied() {
                control.check()?;
                if rows.last().is_some_and(|row| row.id == id) {
                    continue;
                }
                rows.reserve(1)?;
                let (present, document) = if fields.is_empty() {
                    (view.contains(id)?, None)
                } else {
                    let document = view.get_retained(id)?;
                    (document.is_some(), document)
                };
                rows.push(Row {
                    id,
                    present,
                    document,
                })?;
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
