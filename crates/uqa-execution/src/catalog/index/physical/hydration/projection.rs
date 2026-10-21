//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read each complete original rebuild batch before validating columns or invoking expressions.

use super::{decode, storage, Rows, ValueIndexBuilder, ValueIndexHydration};
use crate::physical::physical_exec_error;
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::{DocumentStore, ValueIndexKey};

impl ValueIndexHydration<'_> {
    pub(super) fn project_chunk(
        &self,
        ids: &[DocId],
        output: &mut ValueIndexBuilder,
    ) -> Result<(), SQLError> {
        if let ValueIndexKey::Column(column) = self.field {
            return self.project_column(ids, column, output);
        }
        let captured = {
            let documents = self.documents.read();
            let mut rows = Rows::new(self.work_mem_bytes / 4)?;
            for &id in ids {
                self.control.check().map_err(storage)?;
                let document = documents.get(id).map_err(storage)?;
                rows.push(id, document.map_or(Value::Null, Value::Map))?;
            }
            rows.finish()?
        };
        // Expression callbacks may reenter the table; no document guard survives the capture boundary.
        for row in captured.read_rows().map_err(physical_exec_error)? {
            self.control.check().map_err(storage)?;
            let (id, document) = decode(row.map_err(physical_exec_error)?.row)?;
            let Value::Map(document) = document else {
                if matches!(document, Value::Null) {
                    continue;
                }
                return Err(invalid("invalid captured index document"));
            };
            let mut values = self.definitions.document_values(
                self.expressions,
                self.table,
                std::slice::from_ref(self.field),
                &document,
            )?;
            let value = values
                .remove(self.field)
                .ok_or_else(|| invalid("missing prepared physical key"))?;
            output.push(id, value, self.control)?;
        }
        Ok(())
    }

    fn project_column(
        &self,
        ids: &[DocId],
        column: &str,
        output: &mut ValueIndexBuilder,
    ) -> Result<(), SQLError> {
        let documents = self.documents.read();
        let mut captured = Rows::new(self.work_mem_bytes / 4)?;
        self.capture_column(&***documents, ids, column, &mut captured)?;
        let captured = captured.finish()?;
        for row in captured.read_rows().map_err(physical_exec_error)? {
            self.control.check().map_err(storage)?;
            let (id, projection) = decode(row.map_err(physical_exec_error)?.row)?;
            match projection {
                Value::Null if documents.get(id).map_err(storage)?.is_none() => {}
                Value::Null => {
                    return Err(invalid(format!(
                        "value-index rebuild for {} lost document {id}",
                        self.table
                    )));
                }
                Value::List(mut values) if values.len() == 1 => {
                    output.push(id, values.pop().expect("one field"), self.control)?;
                }
                Value::List(values) => {
                    return Err(invalid(format!(
                        "value-index rebuild for {} returned {} fields; expected 1",
                        self.table,
                        values.len()
                    )));
                }
                _ => return Err(invalid("invalid captured column projection")),
            }
        }
        Ok(())
    }

    fn capture_column(
        &self,
        documents: &dyn DocumentStore,
        ids: &[DocId],
        column: &str,
        captured: &mut Rows,
    ) -> Result<(), SQLError> {
        let mut next = 0;
        let mut failure = None;
        let visited =
            documents.for_each_fields_multi_borrowed(ids, &[column], &mut |id, exists, values| {
                let result = (|| {
                    self.control.check().map_err(storage)?;
                    if ids.get(next) != Some(&id) {
                        return Err(invalid("index projection changed identity order"));
                    }
                    next += 1;
                    captured.push(
                        id,
                        if exists {
                            Value::List(values.iter().map(|value| (*value).clone()).collect())
                        } else {
                            Value::Null
                        },
                    )
                })();
                if let Err(error) = result {
                    failure = Some(error);
                    false
                } else {
                    true
                }
            });
        if let Some(error) = failure {
            return Err(error);
        }
        match visited.map_err(storage)? {
            Some(count) if count == ids.len() && next == count => return Ok(()),
            Some(_) => return Err(invalid("index projection returned an incomplete batch")),
            None if next != 0 => return Err(invalid("declined index projection visited a row")),
            None => {}
        }
        for &id in ids {
            self.control.check().map_err(storage)?;
            let mut values = documents
                .get_fields_multi(std::slice::from_ref(&id), &[column])
                .map_err(storage)?;
            captured.push(id, values.remove(&id).map_or(Value::Null, Value::List))?;
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> SQLError {
    SQLError::Internal(message.into())
}
