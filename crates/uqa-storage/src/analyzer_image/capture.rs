//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable analyzer images keep detached encoded resources and original capture controls.

use super::write_parts;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::{io::Write, sync::Arc};
use uqa_analysis::{AnalyzerImageSource, CapturedAnalyzer, CompiledAnalyzer};
use uqa_core::memory::{Budgeted, MemoryReservation, ProductionControl};

struct RetainedAnalyzer {
    source: Source,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

enum Source {
    Copied(Budgeted<CapturedAnalyzer>),
    Native(AnalyzerImageSource),
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
            source: Source::Copied(source),
            control: control.clone(),
            _memory: memory,
        })))
    }

    /// Retain a previously published encoded owner without copying its descriptor or dictionaries.
    pub fn capture_source(
        source: &AnalyzerImageSource,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let bytes = Self::wrapper_bytes()
            .checked_add(source.retained_bytes())
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        Ok(Self::from_admitted(source.clone(), control, memory))
    }

    pub(crate) fn wrapper_bytes() -> usize {
        size_of::<RetainedAnalyzer>()
    }

    pub(crate) fn from_admitted(
        source: AnalyzerImageSource,
        control: &StorageReadControl,
        memory: MemoryReservation,
    ) -> Self {
        Self(Arc::new(RetainedAnalyzer {
            source: Source::Native(source),
            control: control.clone(),
            _memory: memory,
        }))
    }

    pub fn write_image(
        &self,
        writer: &mut dyn Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        crate::image_io::with_retained_control(writer, Some(&self.0.control), |writer| match &self
            .0
            .source
        {
            Source::Copied(source) => write_parts(
                source.canonical_json(),
                source.dictionary_resources().len(),
                source.dictionary_resources(),
                writer,
                control,
            ),
            Source::Native(source) => write_parts(
                source.canonical_json(),
                source.dictionary_resources().len(),
                source.dictionary_resources(),
                writer,
                control,
            ),
        })
    }
}

#[cfg(test)]
mod tests;
