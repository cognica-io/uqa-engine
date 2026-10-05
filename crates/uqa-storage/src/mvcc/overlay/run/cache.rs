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
    readers: usize,
    block: Option<(usize, Arc<BudgetedVec<u8>>)>,
    chunk: Option<(u64, Arc<BudgetedVec<u8>>)>,
}

/// Shares the last decrypted block and value chunk among active readers. The last reader releases both; retaining an immutable run or savepoint alone does not retain decoded bytes. Cached bytes remain charged to the transaction that owns the run.
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

    pub(super) fn reader(self: &Arc<Self>) -> RunCacheReader {
        self.cached.lock().readers += 1;
        RunCacheReader(Arc::clone(self))
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

    /// A reader changing blocks no longer needs the cache's ownership of the previous block. Other active readers retain their own shared bytes.
    pub(super) fn discard_other_block(&self, index: usize) {
        let mut cached = self.cached.lock();
        if cached
            .block
            .as_ref()
            .is_some_and(|(previous, _)| *previous != index)
        {
            cached.block = None;
        }
    }

    pub(super) fn keep_block(&self, index: usize, bytes: &Arc<BudgetedVec<u8>>) {
        let mut cached = self.cached.lock();
        if cached.readers != 0 {
            cached.block = Some((index, Arc::clone(bytes)));
        }
    }

    pub(super) fn chunk(&self, start: u64) -> Option<Arc<BudgetedVec<u8>>> {
        let mut cached = self.cached.lock();
        if cached
            .chunk
            .as_ref()
            .is_some_and(|(previous, _)| *previous != start)
        {
            cached.chunk = None;
        }
        cached
            .chunk
            .as_ref()
            .filter(|(cached, _)| *cached == start)
            .map(|(_, bytes)| Arc::clone(bytes))
    }

    pub(super) fn keep_chunk(&self, start: u64, bytes: &Arc<BudgetedVec<u8>>) {
        let mut cached = self.cached.lock();
        if cached.readers != 0 {
            cached.chunk = Some((start, Arc::clone(bytes)));
        }
    }
}

/// Keeps shared decoded bytes only for the duration of a scan or lookup operation.
pub(in crate::mvcc) struct RunCacheReader(Arc<RunCache>);

impl Drop for RunCacheReader {
    fn drop(&mut self) {
        let mut cached = self.0.cached.lock();
        cached.readers -= 1;
        if cached.readers == 0 {
            cached.block = None;
            cached.chunk = None;
        }
    }
}
