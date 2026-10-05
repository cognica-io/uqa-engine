//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replay original and rewritten documents without retaining an unbounded row or identity vector.

use crate::{physical::physical_exec_error, spill::BufferedIndexedSpill, PhysicalRow, RowSchema};
use uqa_core::{memory::MemoryBudget, DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

pub struct RewriteRow {
    pub original_id: DocId,
    pub target_id: DocId,
    pub document: Document,
}

/// Original inputs, converted outputs and identity-remapped outputs share the same resident allowance. Encrypted spill retains exact value carriers and full-width document identities; replay holds one decoded row at a time.
pub struct RewriteRows {
    rows: BufferedIndexedSpill,
}

impl RewriteRows {
    pub fn new(memory: &MemoryBudget) -> Self {
        let relation = uqa_sql::ast::InternalRelationId::allocate();
        Self {
            rows: BufferedIndexedSpill::with_memory(
                RowSchema::with_internal_relation_types(relation, vec![None; 3]),
                memory,
            ),
        }
    }

    pub fn memory(&self) -> &MemoryBudget {
        self.rows.memory()
    }

    pub fn len(&self) -> u64 {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn push(&mut self, original_id: DocId, document: Document) -> Result<(), SQLError> {
        self.push_replacement(original_id, original_id, document)
    }

    pub fn push_replacement(
        &mut self,
        original_id: DocId,
        target_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.rows
            .push(&PhysicalRow::from_values(vec![
                Value::Bytes(original_id.to_le_bytes().to_vec()),
                Value::Bytes(target_id.to_le_bytes().to_vec()),
                Value::Map(document),
            ]))
            .map_err(physical_exec_error)
    }

    pub fn get(&mut self, position: u64) -> Result<RewriteRow, SQLError> {
        let mut values = self
            .rows
            .get(position)
            .map_err(physical_exec_error)?
            .into_physical_values()
            .into_iter();
        let original_id = decode_identity(values.next())?;
        let target_id = decode_identity(values.next())?;
        let Some(Value::Map(document)) = values.next() else {
            return Err(SQLError::Internal("rewritten row lost its document".into()));
        };
        Ok(RewriteRow {
            original_id,
            target_id,
            document,
        })
    }

    pub fn spill(&mut self) -> Result<(), SQLError> {
        self.rows.spill().map_err(physical_exec_error)
    }
}

/// Seal the input before evaluating callbacks or publishing any changed declaration.
pub fn capture(
    reads: &dyn crate::mutation::constraints::context::MutationRead,
    table: &str,
    memory: &MemoryBudget,
    control: &uqa_storage::read_control::StorageReadControl,
) -> Result<RewriteRows, SQLError> {
    let mut result = RewriteRows::new(memory);
    let mut after = None;
    let limit = (control.memory().limit() / (4 * std::mem::size_of::<DocId>()))
        .clamp(1, crate::DEFAULT_BATCH_SIZE);
    loop {
        control.cancellation().check()?;
        let ids = reads.live_table_doc_id_page(table, after, limit, control)?;
        let Some(last) = ids.last().copied() else {
            break;
        };
        after = Some(last);
        for id in ids.iter().copied() {
            control.cancellation().check()?;
            if let Some(document) = reads.get_document(table, id)? {
                result.push(id, document)?;
            }
        }
    }
    Ok(result)
}

fn decode_identity(value: Option<Value>) -> Result<DocId, SQLError> {
    let Some(Value::Bytes(bytes)) = value else {
        return Err(SQLError::Internal("rewritten row lost its identity".into()));
    };
    let bytes = bytes
        .try_into()
        .map_err(|_| SQLError::Internal("rewritten row has an invalid identity".into()))?;
    Ok(DocId::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests;
