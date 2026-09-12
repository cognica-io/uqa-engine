//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-owned provider reads share allocation limits and cancellation with their consumers.

use crate::StorageBackendResult;
use uqa_core::{memory::MemoryBudget, CancellationToken};

pub type ValueReadVisitor<'a> = dyn FnMut(Option<&[u8]>) -> StorageBackendResult<()> + 'a;
pub type KeyValueReadVisitor<'a> = dyn FnMut(&[u8], &[u8]) -> StorageBackendResult<()> + 'a;

#[derive(Clone, Debug)]
pub struct StorageReadControl {
    memory: MemoryBudget,
    cancellation: CancellationToken,
}
impl StorageReadControl {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            memory: MemoryBudget::new(limit),
            cancellation: CancellationToken::new(),
        }
    }
    pub fn new(memory: &MemoryBudget, cancellation: &CancellationToken) -> Self {
        Self {
            memory: memory.clone(),
            cancellation: cancellation.clone(),
        }
    }
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
    pub fn check(&self) -> StorageBackendResult<()> {
        self.cancellation.check()?;
        Ok(())
    }
}
