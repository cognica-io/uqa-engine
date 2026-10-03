//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The most recently read block of a run's entries and chunk of its values.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedVec, MemoryBudget};

#[derive(Default)]
struct Cached {
    block: Option<(usize, Arc<BudgetedVec<u8>>)>,
    chunk: Option<(u64, Arc<BudgetedVec<u8>>)>,
}

/// Keeps the entry block and the value chunk a run last decrypted, so that reads of neighbouring keys, which ordered readers make one after another, decrypt each block once. The cached bytes are charged to the allowance of the transaction that owns the run.
pub(super) struct RunCache {
    memory: MemoryBudget,
    cached: Mutex<Cached>,
}

impl RunCache {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            memory: memory.clone(),
            cached: Mutex::new(Cached::default()),
        }
    }

    /// The allowance cached bytes are charged to.
    pub(super) fn memory(&self) -> &MemoryBudget {
        &self.memory
    }

    pub(super) fn block(&self, index: usize) -> Option<Arc<BudgetedVec<u8>>> {
        self.cached
            .lock()
            .block
            .as_ref()
            .filter(|(cached, _)| *cached == index)
            .map(|(_, bytes)| Arc::clone(bytes))
    }

    pub(super) fn keep_block(&self, index: usize, bytes: &Arc<BudgetedVec<u8>>) {
        self.cached.lock().block = Some((index, Arc::clone(bytes)));
    }

    pub(super) fn chunk(&self, start: u64) -> Option<Arc<BudgetedVec<u8>>> {
        self.cached
            .lock()
            .chunk
            .as_ref()
            .filter(|(cached, _)| *cached == start)
            .map(|(_, bytes)| Arc::clone(bytes))
    }

    pub(super) fn keep_chunk(&self, start: u64, bytes: &Arc<BudgetedVec<u8>>) {
        self.cached.lock().chunk = Some((start, Arc::clone(bytes)));
    }
}
