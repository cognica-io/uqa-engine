//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture evaluated keys before physically sorting and publishing their retained owner.

use super::{observe, storage, value_bytes, ColumnValueIndex, ValueIndexCache};
use crate::{
    external_sort::ExternalSort,
    physical::physical_exec_error,
    spill::{SpillBatchWriter, SpillBuffer},
    spill_scan::SpillScan,
    PhysicalOperator, PhysicalRow, RowSchema,
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::{
    btree_index::retained::RetainedValueIndex, read_control::StorageReadControl,
    StorageBackendError,
};

pub struct ValueIndexBuilder {
    field: String,
    carried: bool,
    limit: usize,
    bytes: usize,
    count: usize,
    policy: ColumnValueIndex,
    schema: RowSchema,
    rows: SpillBuffer,
    writer: SpillBatchWriter,
}

impl ValueIndexBuilder {
    pub fn from_values(
        field: &str,
        carried: bool,
        bytes: usize,
        values: impl IntoIterator<Item = (DocId, Value)>,
        control: &StorageReadControl,
    ) -> Result<ValueIndexCache, SQLError> {
        let mut builder = Self::new(field, carried, bytes)?;
        for (id, value) in values {
            builder.push(id, value, control)?;
        }
        builder.finish(control)
    }

    pub fn new(field: &str, carried: bool, bytes: usize) -> Result<Self, SQLError> {
        let schema = RowSchema::with_internal_relation_types(
            uqa_sql::ast::InternalRelationId::allocate(),
            vec![None; 2],
        );
        Ok(Self {
            field: field.into(),
            carried,
            limit: bytes,
            bytes: 0,
            count: 0,
            policy: ColumnValueIndex::build(field, std::iter::empty()),
            rows: SpillBuffer::new(bytes / 4),
            writer: SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?,
            schema,
        })
    }

    /// Identities must be distinct. Capture invokes no SQL expression or comparison.
    pub fn push(
        &mut self,
        id: DocId,
        value: Value,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        control.check().map_err(storage)?;
        self.bytes = self
            .bytes
            .checked_add(value_bytes(&value, control)?)
            .ok_or_else(|| storage(uqa_core::memory::MemoryError::SizeOverflow.into()))?;
        observe(&mut self.policy, &value);
        self.writer
            .push(
                &mut self.rows,
                PhysicalRow::from_values(vec![Value::Bytes(id.to_be_bytes().to_vec()), value]),
            )
            .map_err(physical_exec_error)?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| storage(uqa_core::memory::MemoryError::SizeOverflow.into()))?;
        Ok(())
    }

    pub fn finish(mut self, control: &StorageReadControl) -> Result<ValueIndexCache, SQLError> {
        control.check().map_err(storage)?;
        self.writer
            .finish(&mut self.rows)
            .map_err(physical_exec_error)?;
        let retained = if self.bytes <= self.limit && !self.rows.has_spilled() {
            for batch in self.rows.drain().map_err(physical_exec_error)? {
                for row in batch.map_err(physical_exec_error)?.rows {
                    let (id, value) = decode(row)?;
                    if self.policy.contains(id) {
                        return Err(SQLError::Internal("duplicate stored index identity".into()));
                    }
                    self.policy.insert(id, &value);
                }
            }
            None
        } else {
            let mut sorted = ExternalSort::physical_columns(
                Box::new(SpillScan::new(self.schema, self.rows)),
                vec![1, 0],
                self.limit / 4,
                control.cancellation(),
            )
            .map_err(physical_exec_error)?;
            sorted.open().map_err(physical_exec_error)?;
            let mut batch = Vec::new().into_iter();
            let result = RetainedValueIndex::from_ordered(
                self.limit,
                self.count,
                &mut || loop {
                    control.check()?;
                    if let Some(row) = batch.next() {
                        return decode(row)
                            .map(Some)
                            .map_err(|error| StorageBackendError::backend("stored key", error));
                    }
                    let next = sorted
                        .next()
                        .map_err(physical_exec_error)
                        .map_err(|error| StorageBackendError::backend("stored key", error))?;
                    let Some(next) = next else { return Ok(None) };
                    batch = next.rows.into_iter();
                },
                control,
            )
            .map_err(storage);
            let cleanup = sorted.close().map_err(physical_exec_error);
            let values = result?;
            cleanup?;
            Some(values)
        };
        if self.carried {
            // Keep observed flags even for carried values so a later search-key declaration need not rescan their population.
            self.policy.index = None;
            self.policy.nulls.clear();
        }
        Ok(ValueIndexCache {
            policy: self.policy,
            retained,
            field: self.field,
            limit: self.limit,
            resident_bytes: self.bytes,
        })
    }
}

fn decode(row: PhysicalRow) -> Result<(DocId, Value), SQLError> {
    let Some(Value::Bytes(bytes)) = row.value(0) else {
        return Err(SQLError::Internal("invalid stored index identity".into()));
    };
    let id = u64::from_be_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| SQLError::Internal("invalid stored index identity width".into()))?,
    );
    let value = row
        .value(1)
        .cloned()
        .ok_or_else(|| SQLError::Internal("missing stored index key".into()))?;
    Ok((id, value))
}
