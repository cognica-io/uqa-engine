//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture index-only rows before invoking consumers that may reenter the source.

use super::ScoredDocumentSource;
use crate::{
    mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id},
    physical::physical_exec_error,
    spill::{SpillBatchWriter, SpillBuffer},
    PhysicalRow, RowSchema,
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;

impl ScoredDocumentSource {
    pub(super) fn for_each_indexed_entry(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        allow_borrowed: bool,
        consume: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> Result<Option<usize>, SQLError> {
        if allow_borrowed {
            return self.table.for_each_indexed_fields(
                doc_ids,
                fields,
                &self.index_read_control,
                consume,
            );
        }
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None; 2],
        );
        let mut rows = SpillBuffer::new(self.scan_batch_bytes / 4);
        let mut writer = SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?;
        let mut error = None;
        let result = self.table.for_each_indexed_fields(
            doc_ids,
            fields,
            &self.index_read_control,
            &mut |id, exists, values| {
                let value = if exists {
                    Value::List(values.iter().map(|value| (*value).clone()).collect())
                } else {
                    Value::Null
                };
                if let Err(failure) = writer.push(
                    &mut rows,
                    PhysicalRow::from_values(vec![encode_prepared_doc_id(id), value]),
                ) {
                    error = Some(physical_exec_error(failure));
                    return false;
                }
                true
            },
        );
        if let Some(error) = error {
            return Err(error);
        }
        let Some(_) = result? else { return Ok(None) };
        writer.finish(&mut rows).map_err(physical_exec_error)?;
        let rows = rows.into_shared(schema).map_err(physical_exec_error)?;
        let mut visited = 0;
        for row in rows.read_rows().map_err(physical_exec_error)? {
            self.index_read_control.check().map_err(|error| {
                crate::storage_errors::storage_error("read captured index projection", &error)
            })?;
            let [id, value] = row
                .map_err(physical_exec_error)?
                .row
                .into_physical_values()
                .try_into()
                .map_err(|_| {
                    SQLError::Internal("invalid captured index projection width".into())
                })?;
            let id = decode_prepared_doc_id(id, "captured index projection")?;
            let (exists, values) = match value {
                Value::List(values) => (true, values),
                Value::Null => (false, Vec::new()),
                _ => {
                    return Err(SQLError::Internal(
                        "invalid captured index projection".into(),
                    ))
                }
            };
            visited += 1;
            if !consume(id, exists, &values.iter().collect::<Vec<_>>()) {
                break;
            }
        }
        Ok(Some(visited))
    }
}
