//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture retains a paired native generation; durable membership is established at publication.

use crate::catalog::foreign::{registry::CapturedForeignCatalog, ForeignCatalog};
use uqa_core::memory::RetainedValue;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::SQLError;
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub struct ForeignTableCaptureSource {
    pub catalog: RetainedValue<ForeignCatalog>,
}

impl ForeignTableCaptureSource {
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedForeignCatalog>, SQLError> {
        self.catalog.capture(control)
    }
}

impl MemoryCaptureSource for ForeignTableCaptureSource {
    type Frozen = CapturedMemoryValue<CapturedForeignCatalog>;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = self
            .capture(&production)
            .map_err(|error| StorageBackendError::backend("foreign catalog capture", error))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("foreign capture uses admitted production"));
        CapturedMemoryValue::new(value, control)
    }
}

#[cfg(test)]
mod tests;
