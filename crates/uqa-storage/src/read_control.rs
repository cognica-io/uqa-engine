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
    temporary_files: Option<crate::temporary_file::FileBudget>,
}
impl StorageReadControl {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            memory: MemoryBudget::new(limit),
            cancellation: CancellationToken::new(),
            temporary_files: None,
        }
    }
    pub fn new(memory: &MemoryBudget, cancellation: &CancellationToken) -> Self {
        Self {
            memory: memory.clone(),
            cancellation: cancellation.clone(),
            temporary_files: None,
        }
    }
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Attach a new physical ciphertext allowance to temporary files created by controlled image validation. Clones share this allowance, memory and cancellation; already retained source files keep their existing owner. Without this option, the existing standalone validation has no temporary-file quota.
    pub fn with_temporary_limit(&self, maximum_bytes: u64) -> Self {
        self.with_file_budget(&crate::temporary_file::FileBudget::new(maximum_bytes))
    }

    pub(crate) fn with_file_budget(&self, files: &crate::temporary_file::FileBudget) -> Self {
        Self {
            temporary_files: Some(files.clone()),
            ..self.clone()
        }
    }

    pub(crate) fn file_budget(&self) -> Option<&crate::temporary_file::FileBudget> {
        self.temporary_files.as_ref()
    }
    pub(crate) fn shares_context(&self, other: &Self) -> bool {
        self.memory.shares_allowance(&other.memory)
            && self.cancellation.shares_signal(&other.cancellation)
            && match (&self.temporary_files, &other.temporary_files) {
                (None, None) => true,
                (Some(left), Some(right)) => left.shares_allowance(right),
                _ => false,
            }
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
