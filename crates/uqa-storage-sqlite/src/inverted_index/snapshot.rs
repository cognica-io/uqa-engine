//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical readers retain admitted analyzer metadata on the caller's connection boundary.

use super::{Arc, InvertedIndex, SQLiteInvertedIndex, StorageBackendResult};
use uqa_core::memory::{Budgeted, BudgetedString};
use uqa_storage::{
    inverted_index::{AnalyzerBindings, RetainedAnalyzerBindings},
    read_control::StorageReadControl,
    ReadOnlySnapshot,
};

pub(super) enum IndexBindings {
    Live(AnalyzerBindings),
    Retained(RetainedAnalyzerBindings),
}

impl Clone for IndexBindings {
    fn clone(&self) -> Self {
        Self::Live((**self).clone())
    }
}

impl From<AnalyzerBindings> for IndexBindings {
    fn from(bindings: AnalyzerBindings) -> Self {
        Self::Live(bindings)
    }
}

impl std::ops::Deref for IndexBindings {
    type Target = AnalyzerBindings;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Live(bindings) => bindings,
            Self::Retained(bindings) => bindings,
        }
    }
}

impl IndexBindings {
    pub(super) fn live_mut(&mut self) -> &mut AnalyzerBindings {
        let Self::Live(bindings) = self else {
            unreachable!("immutable physical snapshots are only exposed through read-only handles")
        };
        bindings
    }
}

impl SQLiteInvertedIndex {
    pub(super) fn physical_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        control.check()?;
        let bindings = RetainedAnalyzerBindings::capture(&self.bindings, control)?;
        let mut table = BudgetedString::new(control.memory());
        table.reserve(self.table.len())?;
        for (offset, character) in self.table.chars().enumerate() {
            if offset % 1024 == 0 {
                control.check()?;
            }
            table.push(character)?;
        }
        let (table, mut memory) = table.into_parts();
        memory.grow(size_of::<ReadOnlySnapshot<dyn InvertedIndex>>())?;
        let snapshot = Self {
            conn: self.conn.clone(),
            table,
            bindings: IndexBindings::Retained(bindings),
            retention_control: control.clone(),
        };
        let snapshot = ReadOnlySnapshot::from_budgeted_inverted(Budgeted::new(snapshot, memory))?
            .with_inverted_read_control(control)?;
        Ok(Arc::new(snapshot))
    }
}
