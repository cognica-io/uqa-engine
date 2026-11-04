//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable analyzer images keep detached encoded resources and original capture controls.

use super::write_parts;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::{io::Write, sync::Arc};
use uqa_analysis::{CapturedAnalyzer, CompiledAnalyzer};
use uqa_core::memory::{Budgeted, MemoryReservation, ProductionControl};

struct RetainedAnalyzer {
    source: Budgeted<CapturedAnalyzer>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

#[derive(Clone)]
pub struct AnalyzerImageSnapshot(Arc<RetainedAnalyzer>);

impl AnalyzerImageSnapshot {
    pub fn capture(
        source: &CompiledAnalyzer,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<RetainedAnalyzer>())?;
        let source = source
            .capture_image_with_control(&ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            ))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("analyzer capture owns an allowance"));
        control.check()?;
        Ok(Self(Arc::new(RetainedAnalyzer {
            source,
            control: control.clone(),
            _memory: memory,
        })))
    }

    pub fn write_image(
        &self,
        writer: &mut dyn Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        crate::image_io::with_retained_control(writer, Some(&self.0.control), |writer| {
            let source = &self.0.source;
            write_parts(
                source.canonical_json(),
                source.dictionary_resources().len(),
                source.dictionary_resources(),
                writer,
                control,
            )
        })
    }
}

#[cfg(test)]
mod tests;
