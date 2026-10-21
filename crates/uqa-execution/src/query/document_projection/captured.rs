//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture raw projected inputs before generated evaluation and retain persisted/private phase order.

use crate::{
    physical::physical_exec_error,
    query::{
        document_changes::DocumentChanges,
        scored_input::{CapturedScoredEntries, ScoredEntryCapture},
        SelectedIds,
    },
    spill::{SharedSpillReader, SpillBatchWriter, SpillRows},
    PhysicalRow, RowSchema, SpillBuffer,
};
use uqa_core::{CancellationToken, DocId, Predicate, ScoredEntry, Value};
use uqa_sql::{ast::ColumnDef, semantics::XMIN_COLUMN, SQLError};
use uqa_storage::{DocumentMetadata, DocumentStore};

#[cfg(test)]
mod tests;

pub struct FilterProjection<'a> {
    pub source: &'a dyn DocumentStore,
    pub changes: Option<&'a DocumentChanges>,
    pub definitions: &'a [ColumnDef],
}

#[derive(Default)]
struct Diagnostics {
    missing: Option<DocId>,
    malformed: Option<(DocId, usize)>,
}

impl FilterProjection<'_> {
    pub fn capture(
        &self,
        table: &str,
        ids: &SelectedIds,
        field: &str,
        predicate: &Predicate,
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<CapturedScoredEntries, SQLError> {
        cancellation.check()?;
        let mut diagnostics = Diagnostics::default();
        // The ordinary projection classifies every identity before reading either source.
        let (private, persisted) = ids.partition(bytes / 4, |id| {
            cancellation.check()?;
            uqa_sql::expr::check_statement_cancellation()?;
            let presence = self
                .changes
                .map(|changes| changes.change_presence(id))
                .transpose()
                .map_err(|error| {
                    crate::storage_errors::storage_error("read private document projection", &error)
                })?
                .flatten();
            Ok(match presence {
                Some(false) => {
                    diagnostics.missing = Some(diagnostics.missing.map_or(id, |old| old.min(id)));
                    None
                }
                present => Some(present == Some(true)),
            })
        })?;
        let matched = SelectedIds::capture_visit(bytes / 4, cancellation, |visit| {
            for (selected, source) in [
                (&persisted, Some(self.source)),
                (
                    &private,
                    self.changes.map(|changes| changes as &dyn DocumentStore),
                ),
            ] {
                let Some(source) = source else { continue };
                let projected = Projection {
                    source,
                    definitions: self.definitions,
                    field,
                    cancellation,
                };
                projected.capture(
                    selected.cursor(),
                    predicate,
                    bytes / 4,
                    &mut diagnostics,
                    visit,
                )?;
            }
            Ok(())
        })?;
        if let Some((id, width)) = diagnostics.malformed {
            return Err(SQLError::Internal(format!(
                "read document field returned {width} projected values for document {id}; expected 1"
            )));
        }
        if let Some(id) = diagnostics.missing {
            return Err(SQLError::Internal(format!(
                "Filter consistency error: candidate {id} is missing from the document-field snapshot for table `{table}`"
            )));
        }
        let mut output = ScoredEntryCapture::new(bytes / 4, cancellation.clone())?;
        for id in matched.cursor() {
            output.push(ScoredEntry {
                doc_id: id?,
                score: 0.0,
            })?;
        }
        output.finish()
    }
}

struct Projection<'a> {
    source: &'a dyn DocumentStore,
    definitions: &'a [ColumnDef],
    field: &'a str,
    cancellation: &'a CancellationToken,
}

impl Projection<'_> {
    fn capture(
        &self,
        ids: impl Iterator<Item = Result<DocId, SQLError>>,
        predicate: &Predicate,
        bytes: usize,
        diagnostics: &mut Diagnostics,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let requested = [self.field.to_owned()];
        let generated = super::super::generated::projection_contains_virtual_generated_column(
            self.definitions,
            &requested,
        );
        let xmin = super::projection_uses_tuple_xmin(self.field, self.definitions);
        if !generated && !xmin {
            // Stored-value predicates are pure and infallible. Consume each value directly while retaining all output until both source phases finish.
            for id in ids {
                let id = id?;
                let Some(raw) = self.read_raw(id, false)? else {
                    diagnostics.missing = Some(diagnostics.missing.map_or(id, |old| old.min(id)));
                    continue;
                };
                if let Some(value) = self.stored_value(id, raw, false, diagnostics)? {
                    if predicate.evaluate(Some(&value)) {
                        visit(id)?;
                    }
                }
            }
            return Ok(());
        }
        let raw = self.capture_raw(ids, generated, bytes, diagnostics)?;
        // All storage reads precede generated expressions and tuple-metadata reads, as in read_document_projection.
        for row in raw {
            self.cancellation.check()?;
            uqa_sql::expr::check_statement_cancellation()?;
            let [id, raw] = row
                .map_err(physical_exec_error)?
                .row
                .into_physical_values()
                .try_into()
                .map_err(|_| SQLError::Internal("invalid captured field width".into()))?;
            let id = crate::mutation::prepared::decode_prepared_doc_id(id, "captured field")?;
            let value = if generated {
                let Value::Map(mut document) = raw else {
                    return Err(SQLError::Internal(
                        "invalid captured generated document".into(),
                    ));
                };
                super::super::generated::materialize_projected_virtual_generated_columns(
                    self.definitions,
                    &mut document,
                    &requested,
                )?;
                document.remove(self.field).unwrap_or(Value::Null)
            } else {
                let Some(value) = self.stored_value(id, raw, xmin, diagnostics)? else {
                    continue;
                };
                value
            };
            if predicate.evaluate(Some(&value)) {
                visit(id)?;
            }
        }
        Ok(())
    }

    fn capture_raw(
        &self,
        ids: impl Iterator<Item = Result<DocId, SQLError>>,
        generated: bool,
        bytes: usize,
        diagnostics: &mut Diagnostics,
    ) -> Result<SpillRows<SharedSpillReader>, SQLError> {
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None, None],
        );
        let mut rows = SpillBuffer::new(bytes / 4);
        let mut writer = SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?;
        for id in ids {
            let id = id?;
            let raw = self.read_raw(id, generated)?;
            match raw {
                Some(raw) => writer
                    .push(
                        &mut rows,
                        PhysicalRow::from_values(vec![
                            crate::mutation::prepared::encode_prepared_doc_id(id),
                            raw,
                        ]),
                    )
                    .map_err(physical_exec_error)?,
                None => {
                    diagnostics.missing = Some(diagnostics.missing.map_or(id, |old| old.min(id)));
                }
            }
        }
        writer.finish(&mut rows).map_err(physical_exec_error)?;
        rows.into_shared(schema)
            .map_err(physical_exec_error)?
            .read_rows()
            .map_err(physical_exec_error)
    }

    fn read_raw(&self, id: DocId, generated: bool) -> Result<Option<Value>, SQLError> {
        self.cancellation.check()?;
        uqa_sql::expr::check_statement_cancellation()?;
        if generated {
            self.source
                .get_stored(id)
                .map_err(|error| {
                    crate::storage_errors::storage_error(
                        "read generated document projection",
                        &error,
                    )
                })
                .map(|document| document.map(|document| Value::Map(document.into_fields())))
        } else {
            self.source
                .get_fields_multi(&[id], &[self.field])
                .map_err(|error| {
                    crate::storage_errors::storage_error("read document projection", &error)
                })
                .map(|mut rows| rows.remove(&id).map(Value::List))
        }
    }

    fn stored_value(
        &self,
        id: DocId,
        raw: Value,
        xmin: bool,
        diagnostics: &mut Diagnostics,
    ) -> Result<Option<Value>, SQLError> {
        let Value::List(mut values) = raw else {
            return Err(SQLError::Internal("invalid captured field values".into()));
        };
        if values.len() != 1 {
            if xmin {
                return Err(SQLError::Internal(format!(
                    "document {id} returned {} fields for a 1-field projection",
                    values.len()
                )));
            }
            if diagnostics.malformed.is_none_or(|(old, _)| id < old) {
                diagnostics.malformed = Some((id, values.len()));
            }
            return Ok(None);
        }
        if xmin {
            self.tuple_xmin(id).map(Some)
        } else {
            Ok(Some(values.remove(0)))
        }
    }

    fn tuple_xmin(&self, id: DocId) -> Result<Value, SQLError> {
        let explicit = if self.definitions.is_empty() {
            self.source.get_field(id, XMIN_COLUMN).map_err(|error| {
                crate::storage_errors::storage_error("read dynamic xmin field", &error)
            })?
        } else {
            None
        };
        match explicit {
            Some(value) => Ok(value),
            None => Ok(self
                .source
                .get_metadata(id)
                .map_err(|error| {
                    crate::storage_errors::storage_error("read projected tuple metadata", &error)
                })?
                .and_then(DocumentMetadata::tuple_xmin)
                .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)))),
        }
    }
}
