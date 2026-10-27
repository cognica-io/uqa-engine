//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered event images retain one complete source schema and bounded encoded rows.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::{CancellationToken, Value};
use uqa_sql::SQLError;

use crate::mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id};
use crate::physical::physical_exec_error;
use crate::spill::{decode_physical_row_record, encode_physical_row_record, BufferedIndexedSpill};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};

use super::OwnedMutationImages;

const WIDTH: usize = 8;
const VERSION: i64 = 1;

pub struct MutationImageBuffer {
    rows: BufferedIndexedSpill,
    context_schema: Option<RowSchema>,
    cancellation: CancellationToken,
}

impl MutationImageBuffer {
    pub fn new(bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            rows: BufferedIndexedSpill::new(RowSchema::new(vec![String::new(); WIDTH]), bytes),
            context_schema: None,
            cancellation,
        }
    }

    pub fn push(&mut self, row: OwnedMutationImages) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if self.rows.len() == usize::MAX as u64 {
            return Err(invalid("event count exceeds the addressable index range"));
        }
        let mut context_schema = None;
        let context = if let Some(context) = row.context {
            if self
                .context_schema
                .as_ref()
                .is_some_and(|schema| *schema != context.schema)
            {
                return Err(invalid("source schema changed within one event query"));
            }
            let encoded = encode_physical_row_record(&context.row, context.schema.physical_width())
                .map_err(physical_exec_error)?;
            context_schema = Some(context.schema);
            Value::Bytes(encoded)
        } else {
            Value::Null
        };
        self.rows
            .push(&PhysicalRow::from_values(vec![
                Value::Int(VERSION),
                row.old_storage_table.map_or(Value::Null, Value::Str),
                row.old_doc_id.map_or(Value::Null, encode_prepared_doc_id),
                row.old.map_or(Value::Null, Value::Map),
                row.new_storage_table.map_or(Value::Null, Value::Str),
                row.new_doc_id.map_or(Value::Null, encode_prepared_doc_id),
                row.new.map_or(Value::Null, Value::Map),
                context,
            ]))
            .map_err(physical_exec_error)?;
        if self.context_schema.is_none() {
            self.context_schema = context_schema;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        usize::try_from(self.rows.len()).expect("push checks the addressable row count")
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn finish(self) -> MutationImages {
        let len = self.len();
        MutationImages(Arc::new(MutationImagesInner {
            rows: Mutex::new(self.rows),
            context_schema: self.context_schema,
            cancellation: self.cancellation,
            len,
        }))
    }
}

struct MutationImagesInner {
    rows: Mutex<BufferedIndexedSpill>,
    context_schema: Option<RowSchema>,
    cancellation: CancellationToken,
    len: usize,
}

/// Shared immutable event ownership; callbacks never run while its positional reader is locked.
#[derive(Clone)]
pub struct MutationImages(Arc<MutationImagesInner>);

impl MutationImages {
    pub fn from_rows(
        rows: impl IntoIterator<Item = Result<OwnedMutationImages, SQLError>>,
        bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut buffer = MutationImageBuffer::new(bytes, cancellation);
        for row in rows {
            buffer.push(row?)?;
        }
        Ok(buffer.finish())
    }

    pub fn len(&self) -> usize {
        self.0.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Result<OwnedMutationImages, SQLError> {
        self.0.cancellation.check()?;
        if index >= self.len() {
            return Err(invalid("source index is outside the event row set"));
        }
        let row = self
            .0
            .rows
            .lock()
            .get(index as u64)
            .map_err(physical_exec_error)?;
        decode(row, self.0.context_schema.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = Result<OwnedMutationImages, SQLError>> + '_ {
        (0..self.len()).map(|index| self.get(index))
    }
}

fn decode(row: PhysicalRow, schema: Option<&RowSchema>) -> Result<OwnedMutationImages, SQLError> {
    let values: [Value; WIDTH] = row
        .into_physical_values()
        .try_into()
        .map_err(|_| invalid("encoded event has an invalid width"))?;
    let [version, old_table, old_id, old, new_table, new_id, new, context] = values;
    if !matches!(version, Value::Int(VERSION)) {
        return Err(invalid("encoded event has an invalid version"));
    }
    let context = match context {
        Value::Null => None,
        Value::Bytes(bytes) => {
            let schema = schema.ok_or_else(|| invalid("source context lost its schema"))?;
            let row = decode_physical_row_record(&bytes, schema.physical_width())
                .map_err(physical_exec_error)?;
            Some(OwnedPhysicalRow::new(schema.clone(), row))
        }
        _ => return Err(invalid("source context is not an encoded physical row")),
    };
    Ok(OwnedMutationImages {
        old_storage_table: optional_table(old_table)?,
        old_doc_id: optional_id(old_id)?,
        old: optional_document(old)?,
        new_storage_table: optional_table(new_table)?,
        new_doc_id: optional_id(new_id)?,
        new: optional_document(new)?,
        context,
    })
}

fn optional_table(value: Value) -> Result<Option<String>, SQLError> {
    match value {
        Value::Null => Ok(None),
        Value::Str(table) => Ok(Some(table)),
        _ => Err(invalid("storage table is not text")),
    }
}

fn optional_id(value: Value) -> Result<Option<uqa_core::DocId>, SQLError> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        decode_prepared_doc_id(value, "rewrite-rule event").map(Some)
    }
}

fn optional_document(
    value: Value,
) -> Result<Option<uqa_storage::document_store::Document>, SQLError> {
    match value {
        Value::Null => Ok(None),
        Value::Map(document) => Ok(Some(document)),
        _ => Err(invalid("event image is not a document")),
    }
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("rewrite-rule {message}"))
}

#[cfg(test)]
mod tests;
