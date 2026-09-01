//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional build-row storage and its atomic memory-to-disk transition.

use crate::{ExecError, ExecResult, IndexedSpill, PhysicalRow, RowSchema};

/// Positional build-side row storage that only touches disk after its encoded
/// memory budget is exhausted. Once spilled, every row lives in the indexed
/// disk store so positional indices remain stable across the transition.
pub(super) struct HybridRowStore {
    pub(super) schema: RowSchema,
    memory: Vec<PhysicalRow>,
    rows: u64,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<IndexedSpill>,
}

impl HybridRowStore {
    pub(super) fn new(schema: RowSchema, budget_bytes: usize) -> Self {
        Self {
            schema,
            memory: Vec::new(),
            rows: 0,
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.disk.as_ref().map_or(self.rows, IndexedSpill::len)
    }

    pub(super) fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    pub(super) fn memory_row(&self, index: u64) -> Option<&PhysicalRow> {
        if self.disk.is_some() {
            return None;
        }
        usize::try_from(index)
            .ok()
            .and_then(|index| self.memory.get(index))
    }

    pub(super) fn push(&mut self, row: PhysicalRow) -> ExecResult<()> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.push(&row);
        }

        let row_bytes = IndexedSpill::encoded_row_size(&self.schema, &row)?;
        let next_rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("join build row count overflow".into()))?;
        let fits = self
            .memory_bytes
            .checked_add(row_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory.push(row);
            self.rows = next_rows;
            self.memory_bytes += row_bytes;
            return Ok(());
        }

        // Build the complete disk representation before publishing it. If any
        // append fails, the original in-memory rows remain available and the
        // operator aborts without exposing a partial positional store.
        let mut disk = IndexedSpill::new(self.schema.clone())?;
        for existing in &self.memory {
            disk.push(existing)?;
        }
        disk.push(&row)?;
        self.memory.clear();
        self.rows = disk.len();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(())
    }

    pub(super) fn with_row<T>(
        &mut self,
        index: u64,
        visitor: impl FnOnce(&PhysicalRow) -> ExecResult<T>,
    ) -> ExecResult<T> {
        if let Some(disk) = self.disk.as_mut() {
            let row = disk.get(index)?;
            return visitor(&row);
        }

        let index = usize::try_from(index)
            .map_err(|_| ExecError::Other(format!("join row index {index} exceeds usize")))?;
        let row = self.memory.get(index).ok_or_else(|| {
            ExecError::Other(format!(
                "join row {index} is outside 0..{}",
                self.memory.len()
            ))
        })?;
        visitor(row)
    }
}
