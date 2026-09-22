//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve requested row order and early termination across base and private projections.

use super::{DocId, DocumentStore, RetainedDocuments, StorageBackendResult, Value};
use crate::query::table_snapshot::layout::RowProjection;

impl RetainedDocuments {
    pub(super) fn visit_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        let base_projection = self.0.layout.projection(fields);
        let private_projection = self.0.private_layout.projection(fields);
        let nulls = vec![&Value::Null; fields.len()];
        let mut index = 0;
        while index < ids.len() {
            self.0.cancellation.check()?;
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
                    index += 1;
                }
                let source: &dyn DocumentStore = if private {
                    &self.0.changes
                } else {
                    self.0.source.as_ref()
                };
                if !self.visit_source_projection(
                    source,
                    &ids[start..index],
                    fields,
                    projection,
                    &nulls,
                    visitor,
                )? {
                    break;
                }
            } else {
                if !self.visit_individual_projection(id, fields, visitor)? {
                    break;
                }
                index += 1;
            }
        }
        Ok(())
    }

    fn visit_individual_projection(
        &self,
        id: DocId,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<bool> {
        let present = self.contains_doc_id(id)?;
        let values = if present {
            fields
                .iter()
                .map(|field| {
                    self.get_field(id, field)
                        .map(|value| value.unwrap_or(Value::Null))
                })
                .collect::<StorageBackendResult<Vec<_>>>()?
        } else {
            vec![Value::Null; fields.len()]
        };
        let values = values.iter().collect::<Vec<_>>();
        Ok(visitor(id, present, &values))
    }

    fn visit_source_projection(
        &self,
        source: &dyn DocumentStore,
        ids: &[DocId],
        fields: &[&str],
        projection: &RowProjection<'_>,
        nulls: &[&Value],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<bool> {
        let mut offset = 0;
        while offset < ids.len() {
            let mut visited = 0;
            let mut ambiguous = None;
            let mut keep_going = true;
            source.for_each_fields_multi_ref_with_presence(
                &ids[offset..],
                &projection.sources,
                &mut |id, present, values| {
                    visited += 1;
                    if present && projection.needs_presence(values) {
                        ambiguous = Some(id);
                        return false;
                    }
                    keep_going = if present {
                        visitor(id, true, &projection.values(values))
                    } else {
                        visitor(id, false, nulls)
                    };
                    keep_going
                },
            )?;
            if !keep_going {
                return Ok(false);
            }
            let Some(id) = ambiguous else {
                break;
            };
            // Release the provider's borrowed-row guard before requesting field-presence information.
            if !self.visit_individual_projection(id, fields, visitor)? {
                return Ok(false);
            }
            offset += visited;
        }
        Ok(true)
    }
}
