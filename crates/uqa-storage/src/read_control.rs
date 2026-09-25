//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-owned provider reads share allocation limits and cancellation with their consumers.

use crate::StorageBackendResult;
use uqa_core::memory::{MemoryBudget, MemoryError};
pub use uqa_core::CancellationToken;

pub type ValueReadVisitor<'a> = dyn FnMut(Option<&[u8]>) -> StorageBackendResult<()> + 'a;
pub type KeyValueReadVisitor<'a> = dyn FnMut(&[u8], &[u8]) -> StorageBackendResult<()> + 'a;
pub type KeyReadVisitor<'a> = dyn FnMut(&[u8]) -> StorageBackendResult<()> + 'a;

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
    pub(crate) fn shares_context(&self, other: &Self) -> bool {
        self.memory.shares_allowance(&other.memory)
            && self.cancellation.shares_signal(&other.cancellation)
    }
    pub fn check(&self) -> StorageBackendResult<()> {
        self.cancellation.check()?;
        Ok(())
    }

    /// Validate an encoded value's size before materializing it. Provider workspace and returned bytes still share this control's memory allowance.
    pub fn check_value_size(&self, bytes: usize, maximum: usize) -> StorageBackendResult<()> {
        self.check()?;
        if bytes > maximum {
            return Err(MemoryError::Limit {
                required: bytes,
                limit: maximum,
            }
            .into());
        }
        Ok(())
    }
}
