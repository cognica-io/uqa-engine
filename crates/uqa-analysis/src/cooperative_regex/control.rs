//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded work checks and reserved initialization for regex execution.

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use crate::AnalysisResult;

pub(super) struct Control<'a> {
    poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
    remaining: usize,
}

impl<'a> Control<'a> {
    pub(super) fn new(poll: &'a mut dyn FnMut() -> AnalysisResult<()>) -> Self {
        Self { poll, remaining: 0 }
    }

    pub(super) fn step(&mut self) -> AnalysisResult<()> {
        self.work(1)
    }

    fn work(&mut self, units: usize) -> AnalysisResult<()> {
        if units > self.remaining {
            (self.poll)()?;
            self.remaining = 1024;
        }
        self.remaining -= units;
        Ok(())
    }

    pub(super) fn copy<T: Copy>(&mut self, source: &[T], target: &mut [T]) -> AnalysisResult<()> {
        for (source, target) in source.chunks(1024).zip(target.chunks_mut(1024)) {
            self.work(source.len())?;
            target.copy_from_slice(source);
        }
        Ok(())
    }

    pub(super) fn fill<T: Copy>(&mut self, target: &mut [T], value: T) -> AnalysisResult<()> {
        for chunk in target.chunks_mut(1024) {
            self.work(chunk.len())?;
            chunk.fill(value);
        }
        Ok(())
    }

    pub(super) fn buffer<T: Copy>(
        &mut self,
        budget: &MemoryBudget,
        len: usize,
        value: T,
    ) -> AnalysisResult<BudgetedVec<T>> {
        (self.poll)()?;
        let mut buffer = BudgetedVec::new(budget);
        buffer.reserve(len)?;
        for _ in 0..len {
            self.step()?;
            buffer.push(value)?;
        }
        Ok(buffer)
    }
}
