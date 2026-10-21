//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded capture of exact values uses the existing positional spill codec.

use crate::{
    mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id},
    physical::physical_exec_error,
    spill::{SpillBatchWriter, SpillBuffer},
    PhysicalRow, RowSchema, SharedSpill,
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;

pub(super) struct Rows {
    schema: RowSchema,
    rows: SpillBuffer,
    writer: SpillBatchWriter,
}

impl Rows {
    pub fn new(bytes: usize) -> Result<Self, SQLError> {
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None; 2],
        );
        Ok(Self {
            writer: SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?,
            schema,
            rows: SpillBuffer::new(bytes),
        })
    }
    pub fn push(&mut self, id: DocId, value: Value) -> Result<(), SQLError> {
        self.writer
            .push(
                &mut self.rows,
                PhysicalRow::from_values(vec![encode_prepared_doc_id(id), value]),
            )
            .map_err(physical_exec_error)
    }
    pub fn finish(mut self) -> Result<SharedSpill, SQLError> {
        self.writer
            .finish(&mut self.rows)
            .map_err(physical_exec_error)?;
        self.rows
            .into_shared(self.schema)
            .map_err(physical_exec_error)
    }
}

pub(super) fn decode(row: PhysicalRow) -> Result<(DocId, Value), SQLError> {
    let [id, value] = row
        .into_physical_values()
        .try_into()
        .map_err(|_| SQLError::Internal("invalid captured index row width".into()))?;
    Ok((decode_prepared_doc_id(id, "captured index row")?, value))
}
