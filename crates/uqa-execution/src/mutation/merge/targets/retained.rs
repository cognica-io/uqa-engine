//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered original-to-current MERGE identities and separately retained row images.

use crate::{
    mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id},
    physical::physical_exec_error,
    spill::BufferedIndexedSpill,
    PhysicalRow, RowSchema,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use super::super::model::MergeTargetIdentity;

pub(super) struct TargetChanges {
    identities: BufferedIndexedSpill,
    documents: BufferedIndexedSpill,
    cancellation: CancellationToken,
}

pub(super) enum RecheckedTarget {
    Deleted,
    Refreshed(MergeTargetIdentity, Document),
}

pub(super) struct TargetRecord {
    pub(super) original: MergeTargetIdentity,
    pub(super) current: Option<MergeTargetIdentity>,
    document: Option<u64>,
}

impl TargetChanges {
    pub(super) fn new(memory: &MemoryBudget, cancellation: CancellationToken) -> Self {
        Self {
            identities: BufferedIndexedSpill::with_memory(
                RowSchema::new(vec![String::new(); 5]),
                memory,
            ),
            documents: BufferedIndexedSpill::with_memory(
                RowSchema::new(vec![String::new()]),
                memory,
            ),
            cancellation,
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.identities.len()
    }

    /// Callers append in the original physical identity order, before any MERGE row effects.
    pub(super) fn push(
        &mut self,
        original: (&str, DocId),
        current: Option<(&str, DocId)>,
        document: Option<Document>,
    ) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let position = if let Some(document) = document {
            let position = self.documents.len();
            self.documents
                .push(&PhysicalRow::from_values(vec![Value::Map(document)]))
                .map_err(physical_exec_error)?;
            encode_prepared_doc_id(position)
        } else {
            Value::Null
        };
        self.identities
            .push(&PhysicalRow::from_values(vec![
                Value::Str(original.0.to_owned()),
                encode_prepared_doc_id(original.1),
                current.map_or(Value::Null, |(table, _)| Value::Str(table.to_owned())),
                current.map_or(Value::Null, |(_, id)| encode_prepared_doc_id(id)),
                position,
            ]))
            .map_err(physical_exec_error)
    }

    pub(super) fn record(&mut self, position: u64) -> Result<TargetRecord, SQLError> {
        self.cancellation.check()?;
        let [table, id, current_table, current_id, document] = self
            .identities
            .get(position)
            .map_err(physical_exec_error)?
            .into_physical_values()
            .try_into()
            .map_err(|_| invalid("identity width"))?;
        let Value::Str(table) = table else {
            return Err(invalid("original table"));
        };
        let original = (table, decode_prepared_doc_id(id, "MERGE target")?);
        let current = match (current_table, current_id) {
            (Value::Null, Value::Null) => None,
            (Value::Str(table), id) => {
                Some((table, decode_prepared_doc_id(id, "MERGE successor")?))
            }
            _ => return Err(invalid("current identity")),
        };
        let document = match document {
            Value::Null => None,
            value => Some(decode_prepared_doc_id(value, "MERGE retained document")?),
        };
        Ok(TargetRecord {
            original,
            current,
            document,
        })
    }

    pub(super) fn lookup(
        &mut self,
        table: &str,
        id: DocId,
    ) -> Result<Option<RecheckedTarget>, SQLError> {
        self.cancellation.check()?;
        let mut lo = 0;
        let mut hi = self.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = self.record(mid)?;
            match (entry.original.0.as_str(), entry.original.1).cmp(&(table, id)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let Some(current) = entry.current else {
                        return Ok(Some(RecheckedTarget::Deleted));
                    };
                    let row = self
                        .documents
                        .get(
                            entry
                                .document
                                .ok_or_else(|| invalid("missing current document"))?,
                        )
                        .map_err(physical_exec_error)?;
                    let values: [Value; 1] = row
                        .into_physical_values()
                        .try_into()
                        .map_err(|_| invalid("document width"))?;
                    let [Value::Map(document)] = values else {
                        return Err(invalid("current document"));
                    };
                    return Ok(Some(RecheckedTarget::Refreshed(current, document)));
                }
            }
        }
        Ok(None)
    }
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("retained MERGE target: {detail}"))
}

#[cfg(test)]
mod tests;
