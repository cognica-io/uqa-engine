//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Requested row order, presence and early termination share the query's workspace allowance.

use super::{DocId, DocumentStore, RetainedDocuments, StorageBackendResult, Value};
use crate::query::table_snapshot::layout::RowProjection;
use uqa_core::memory::BudgetedVec;

impl RetainedDocuments {
    pub(super) fn visit_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.0.control.check()?;
        if ids.is_empty() {
            return Ok(());
        }
        let base_projection = self.0.layout.projection(fields)?;
        let private_projection = self.0.private_layout.projection(fields)?;
        let mut nulls = BudgetedVec::new(self.0.control.memory());
        nulls.reserve(fields.len())?;
        for _ in fields {
            self.0.control.check()?;
            nulls.push(&Value::Null)?;
        }
        let mut index = 0;
        while index < ids.len() {
            self.0.control.check()?;
            let id = ids[index];
            let private = self.0.changes.contains_change(id);
            let projection = if private {
                &private_projection
            } else {
                &base_projection
            };
            if let Some(projection) = projection.as_ref() {
                let start = index;
                while index < ids.len() && self.0.changes.contains_change(ids[index]) == private {
                    self.0.control.check()?;
                    index += 1;
                }
                let source: &dyn DocumentStore = if private {
                    &self.0.changes
                } else {
                    self.0.source.as_ref()
                };
                if !visit_source_projection(
                    &self.0.control,
                    source,
                    &ids[start..index],
                    projection,
                    &nulls,
                    visitor,
                )? {
                    break;
                }
            } else {
                if !self.visit_individual_projection(id, fields, &nulls, visitor)? {
                    break;
                }
                index += 1;
            }
        }
        self.0.control.check()
    }

    fn visit_individual_projection(
        &self,
        id: DocId,
        fields: &[&str],
        nulls: &[&Value],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<bool> {
        self.0.control.check()?;
        let present = self.contains_doc_id(id)?;
        if !present {
            self.0.control.check()?;
            return Ok(visitor(id, false, nulls));
        }
        let mut values = BudgetedVec::new(self.0.control.memory());
        values.reserve(fields.len())?;
        for field in fields {
            self.0.control.check()?;
            values.push(self.get_field(id, field)?.unwrap_or(Value::Null))?;
        }
        let mut projected = BudgetedVec::new(self.0.control.memory());
        projected.reserve(values.len())?;
        for value in values.iter() {
            self.0.control.check()?;
            projected.push(value)?;
        }
        self.0.control.check()?;
        Ok(visitor(id, true, &projected))
    }
}

pub(in crate::query::table_snapshot) fn visit_source_projection(
    control: &uqa_storage::read_control::StorageReadControl,
    source: &dyn DocumentStore,
    ids: &[DocId],
    projection: &RowProjection<'_>,
    nulls: &[&Value],
    visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
) -> StorageBackendResult<bool> {
    let presence = if projection.needs_presence() {
        uqa_storage::document_store::read_field_presence(source, ids, &projection.sources, control)?
    } else {
        BudgetedVec::new(control.memory())
    };
    let mut visited = 0;
    let mut keep_going = true;
    let mut failure = None;
    let read = source.for_each_fields_multi_ref_with_presence(
        ids,
        &projection.sources,
        &mut |id, present, values| {
            if let Err(error) = control.check() {
                failure = Some(error);
                return false;
            }
            if ids.get(visited) != Some(&id) || values.len() != projection.sources.len() {
                failure = Some(super::StorageBackendError::Other(
                    "document projection returned an unexpected identity or field count".into(),
                ));
                return false;
            }
            let fields_present = if projection.needs_presence() {
                let start = visited * projection.sources.len();
                &presence[start..start + projection.sources.len()]
            } else {
                &[]
            };
            visited += 1;
            keep_going = if present {
                match projection.values(values, fields_present) {
                    Ok(projected) => visitor(id, true, &projected),
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            } else {
                visitor(id, false, nulls)
            };
            keep_going
        },
    );
    if let Some(error) = failure {
        return Err(error);
    }
    read?;
    if keep_going && visited != ids.len() {
        return Err(super::StorageBackendError::Other(
            "document projection ended before every requested identity".into(),
        ));
    }
    Ok(keep_going)
}
