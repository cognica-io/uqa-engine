//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded exact keys after SQL has selected and applied foreign-key coercions.

use super::{
    foreign_key_parent_values, foreign_key_scan_tables, ConstraintContext, ForeignKey,
    ForeignKeyComparison,
};
use crate::physical::physical_exec_error;
use crate::spill::BufferedIndexedSpill;
use crate::{
    Batch, PhysicalOperator, PhysicalRow, RowSchema, SharedSpillScan, Sort, SortKey, SpillBuffer,
};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use uqa_core::{CancellationToken, Value};
use uqa_sql::{
    ast::{ColumnType, InternalRelationId},
    SQLError,
};

pub(super) fn foreign_key_parent_index(
    context: ConstraintContext<'_>,
    key: &ForeignKey,
    comparison: &ForeignKeyComparison,
) -> Result<ParentKeys, SQLError> {
    let mut keys = ParentKeyBuffer::new(
        context.memory.work_mem_bytes()?,
        context.cancellation.clone(),
    );
    let control = uqa_storage::read_control::StorageReadControl::new(
        &uqa_core::memory::MemoryBudget::new(64 * 1024),
        context.cancellation,
    );
    for table in foreign_key_scan_tables(context.partitions.catalog, &key.ref_table)? {
        let mut after = None;
        loop {
            let page = context
                .reads
                .table_doc_id_page(&table, after, 128, &control)?;
            let Some(last) = page.last().copied() else {
                break;
            };
            after = Some(last);
            for &id in page.iter() {
                context.cancellation.check()?;
                if let Some(document) = context.reads.get_document(&table, id)? {
                    keys.insert(&foreign_key_parent_values(key, &document, comparison)?)?;
                }
            }
        }
    }
    keys.finish()
}

struct ParentKeyBuffer {
    rows: SpillBuffer,
    payloads: BufferedIndexedSpill,
    schema: RowSchema,
    relation: InternalRelationId,
    bytes: usize,
    cancellation: CancellationToken,
}

impl ParentKeyBuffer {
    fn new(bytes: usize, cancellation: CancellationToken) -> Self {
        let relation = InternalRelationId::allocate();
        Self {
            rows: SpillBuffer::new(bytes / 3),
            payloads: BufferedIndexedSpill::new(
                RowSchema::with_types(vec!["key".into()], vec![Some(ColumnType::Bytea)]),
                bytes / 3,
            ),
            schema: RowSchema::with_internal_relation_types(
                relation,
                vec![Some(ColumnType::Bytea), Some(ColumnType::BigInteger)],
            ),
            relation,
            bytes,
            cancellation,
        }
    }

    fn insert(&mut self, values: &[Value]) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let key = crate::distinct::canonical_row_key(values).map_err(physical_exec_error)?;
        self.cancellation.check()?;
        let digest = Sha256::digest(&key).into();
        self.insert_encoded(key, digest)
    }

    fn insert_encoded(&mut self, key: Vec<u8>, digest: [u8; 32]) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let position = i64::try_from(self.payloads.len())
            .map_err(|_| SQLError::Internal("deferred parent key position overflow".into()))?;
        self.payloads
            .push(&PhysicalRow::from_values(vec![Value::Bytes(key)]))
            .map_err(physical_exec_error)?;
        self.rows
            .push(Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(vec![
                    Value::Bytes(digest.to_vec()),
                    Value::Int(position),
                ])],
            ))
            .map(|_| ())
            .map_err(physical_exec_error)
    }

    fn finish(self) -> Result<ParentKeys, SQLError> {
        self.cancellation.check()?;
        let mut keys = BufferedIndexedSpill::new(self.schema.clone(), self.bytes / 3);
        let mut sorted = Sort::with_evaluator_and_work_mem(
            Box::new(SharedSpillScan::new(
                self.rows
                    .into_shared(self.schema)
                    .map_err(physical_exec_error)?,
            )),
            vec![SortKey {
                expr: crate::ScalarExpr::InternalColumn(self.relation.column(0)),
                descending: false,
                nulls_first: Some(false),
            }],
            std::sync::Arc::new(KeyEvaluator(self.cancellation.clone())),
            self.bytes / (3 * (crate::external_sort::EXTERNAL_SORT_MERGE_FAN_IN + 2)),
        );
        let capture = sorted.open().and_then(|()| {
            capture_sorted(&mut sorted, &mut keys, &self.cancellation)
                .map_err(crate::ExecError::SQL)
        });
        crate::physical::with_cleanup(capture, sorted.close(), "close sorted parent keys")
            .map_err(physical_exec_error)?;
        Ok(ParentKeys {
            rows: RefCell::new(keys),
            payloads: RefCell::new(self.payloads),
            cancellation: self.cancellation,
            #[cfg(test)]
            reads: std::cell::Cell::new(0),
        })
    }
}

fn capture_sorted(
    sorted: &mut Sort<'_>,
    keys: &mut BufferedIndexedSpill,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    while let Some(batch) = sorted.next().map_err(physical_exec_error)? {
        for row in batch.rows {
            cancellation.check()?;
            keys.push(&row).map_err(physical_exec_error)?;
        }
    }
    cancellation.check()?;
    Ok(())
}

pub(super) struct ParentKeys {
    rows: RefCell<BufferedIndexedSpill>,
    payloads: RefCell<BufferedIndexedSpill>,
    cancellation: CancellationToken,
    #[cfg(test)]
    reads: std::cell::Cell<usize>,
}

impl ParentKeys {
    pub(super) fn contains(&self, values: &[Value]) -> Result<bool, SQLError> {
        self.cancellation.check()?;
        let key = crate::distinct::canonical_row_key(values).map_err(physical_exec_error)?;
        let digest = Sha256::digest(&key).into();
        self.contains_encoded(&key, digest)
    }

    fn contains_encoded(&self, key: &[u8], digest: [u8; 32]) -> Result<bool, SQLError> {
        self.cancellation.check()?;
        let mut rows = self.rows.borrow_mut();
        let (mut first, mut end) = (0, rows.len());
        while first < end {
            self.cancellation.check()?;
            let middle = first + (end - first) / 2;
            #[cfg(test)]
            self.reads.set(self.reads.get() + 1);
            let row = rows.get(middle).map_err(physical_exec_error)?;
            let Some(Value::Bytes(candidate)) = row.value(0) else {
                return Err(SQLError::Internal(
                    "deferred parent key lost its encoded value".into(),
                ));
            };
            match candidate.as_slice().cmp(digest.as_slice()) {
                std::cmp::Ordering::Less => first = middle + 1,
                std::cmp::Ordering::Greater | std::cmp::Ordering::Equal => end = middle,
            }
        }
        // Digest equality only locates candidates; every key is compared in full, including deliberately colliding digests.
        while first < rows.len() {
            self.cancellation.check()?;
            let row = rows.get(first).map_err(physical_exec_error)?;
            let Some(Value::Bytes(candidate)) = row.value(0) else {
                return Err(SQLError::Internal(
                    "deferred parent digest is missing".into(),
                ));
            };
            if candidate.as_slice() != digest {
                break;
            }
            let Some(Value::Int(position)) = row.value(1) else {
                return Err(SQLError::Internal(
                    "deferred parent position is missing".into(),
                ));
            };
            let position = u64::try_from(*position)
                .map_err(|_| SQLError::Internal("deferred parent position is negative".into()))?;
            let original = self
                .payloads
                .borrow_mut()
                .get(position)
                .map_err(physical_exec_error)?;
            let Some(Value::Bytes(original)) = original.value(0) else {
                return Err(SQLError::Internal("deferred parent key is missing".into()));
            };
            if original.as_slice() == key {
                return Ok(true);
            }
            first += 1;
        }
        Ok(false)
    }
}

struct KeyEvaluator(CancellationToken);

impl crate::ExpressionEvaluator for KeyEvaluator {
    fn evaluate(
        &self,
        expression: &crate::ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> crate::ExecResult<Value> {
        self.0.check().map_err(SQLError::from)?;
        Ok(crate::eval_scalar(
            expression,
            &crate::ScalarEvalContext::from_row_lookup(row, &[]),
        )?)
    }
}

#[cfg(test)]
mod tests;
