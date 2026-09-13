//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Moving tokens out of a batch transfers attribute leases and frees an exhausted vector promptly.

use uqa_core::memory::{Budgeted, MemoryReservation};

use super::{AllocatedToken, AnalysisToken, TokenBatch};
use crate::AnalysisResult;

pub(crate) struct TokenBatchInput<T = AnalysisToken> {
    tokens: Option<std::vec::IntoIter<T>>,
    terminal: Option<Box<T>>,
    final_position_increment: u32,
    vector_bytes: usize,
    memory: MemoryReservation,
}

impl<T: AllocatedToken> TokenBatchInput<T> {
    pub(super) fn new(batch: TokenBatch<T>, memory: MemoryReservation) -> Self {
        let vector_bytes = batch.tokens.capacity() * size_of::<T>();
        Self {
            tokens: Some(batch.tokens.into_iter()),
            terminal: batch.terminal,
            final_position_increment: batch.final_position_increment,
            vector_bytes,
            memory,
        }
    }

    pub(crate) fn next(
        mut self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<(Option<Budgeted<T>>, Self)> {
        poll()?;
        let Some(tokens) = &mut self.tokens else {
            return Ok((None, self));
        };
        let token = tokens.next();
        if tokens.as_slice().is_empty() {
            drop(self.tokens.take());
            drop(self.memory.split(self.vector_bytes));
            self.vector_bytes = 0;
        }
        let token = token
            .map(|token| -> AnalysisResult<_> {
                let bytes = token.allocation_bytes(poll)?;
                Ok(Budgeted::new(token, self.memory.split(bytes)))
            })
            .transpose()?;
        Ok((token, self))
    }

    pub(crate) fn finish(self) -> (Option<Budgeted<Box<T>>>, u32) {
        assert!(self.tokens.is_none());
        let terminal = self.terminal.map(|token| Budgeted::new(token, self.memory));
        (terminal, self.final_position_increment)
    }
}
