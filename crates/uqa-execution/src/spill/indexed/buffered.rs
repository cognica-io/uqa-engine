//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded positional rows switch atomically to the existing encrypted spill owner.

use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

use super::{
    decode_physical_row_record, encode_physical_row_record, encoded_physical_row_record_size,
    spill_error, ExecResult, IndexedSpill, PhysicalRow, RowSchema,
};

pub(crate) struct BufferedIndexedSpill {
    schema: RowSchema,
    rows: BudgetedVec<Vec<u8>>,
    payload: MemoryReservation,
    disk: Option<IndexedSpill>,
}

impl BufferedIndexedSpill {
    pub(crate) fn new(schema: RowSchema, budget_bytes: usize) -> Self {
        Self::with_memory(schema, &MemoryBudget::new(budget_bytes))
    }

    pub(crate) fn with_memory(schema: RowSchema, memory: &MemoryBudget) -> Self {
        Self {
            schema,
            rows: BudgetedVec::new(memory),
            payload: memory.empty_reservation(),
            disk: None,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.disk.as_ref().map_or_else(
            || u64::try_from(self.rows.len()).expect("row count fits u64"),
            IndexedSpill::len,
        )
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    #[cfg(test)]
    pub(crate) fn spilled_bytes(&self) -> u64 {
        self.disk.as_ref().map_or(0, IndexedSpill::encoded_bytes)
    }

    pub(crate) fn push(&mut self, row: &PhysicalRow) -> ExecResult<()> {
        if let Some(disk) = &mut self.disk {
            return disk.push(row);
        }
        let bytes = encoded_physical_row_record_size(row, self.schema.physical_width())?;
        if self.rows.reserve(1).is_err() {
            return self.spill_and_push(row);
        }
        let Ok(mut memory) = self.rows.budget().reserve(bytes) else {
            return self.spill_and_push(row);
        };
        let record = encode_physical_row_record(row, self.schema.physical_width())?;
        if memory
            .grow(record.capacity().saturating_sub(bytes))
            .is_err()
        {
            drop(record);
            drop(memory);
            return self.spill_and_push(row);
        }
        self.rows
            .push(record)
            .map_err(|error| spill_error(error.to_string()))?;
        self.payload.absorb(memory);
        Ok(())
    }

    pub(crate) fn memory(&self) -> &MemoryBudget {
        self.rows.budget()
    }

    /// Release the resident prefix only after its complete disk representation exists.
    pub(crate) fn spill(&mut self) -> ExecResult<()> {
        if self.disk.is_some() || self.rows.is_empty() {
            return Ok(());
        }
        let mut disk = IndexedSpill::new(self.schema.clone())?;
        for record in self.rows.iter() {
            disk.push_encoded(record)?;
        }
        let memory = self.rows.budget().clone();
        self.rows = BudgetedVec::new(&memory);
        self.payload = memory.empty_reservation();
        self.disk = Some(disk);
        Ok(())
    }

    fn spill_and_push(&mut self, row: &PhysicalRow) -> ExecResult<()> {
        let mut disk = IndexedSpill::new(self.schema.clone())?;
        for record in self.rows.iter() {
            disk.push_encoded(record)?;
        }
        disk.push(row)?;
        // Failed creation or publication leaves every prior in-memory row intact. Release row buffers before their payload allowance only after both spill files contain the complete prefix and incoming row.
        let memory = self.rows.budget().clone();
        self.rows = BudgetedVec::new(&memory);
        self.payload = memory.empty_reservation();
        self.disk = Some(disk);
        Ok(())
    }

    pub(crate) fn get(&mut self, index: u64) -> ExecResult<PhysicalRow> {
        if let Some(disk) = &mut self.disk {
            return disk.get(index);
        }
        let record = usize::try_from(index)
            .ok()
            .and_then(|index| self.rows.get(index))
            .ok_or_else(|| {
                spill_error(format!(
                    "indexed spill row {index} is outside 0..{}",
                    self.rows.len()
                ))
            })?;
        decode_physical_row_record(record, self.schema.physical_width())
    }
}

#[cfg(test)]
mod tests;
