//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed failures at the graph matching boundary.

use std::{collections::TryReserveError, error::Error, fmt};
use uqa_core::QueryCancelled;
use uqa_scoring::TextSearchError;
use uqa_storage::StorageBackendError;

#[derive(Debug)]
pub enum PhraseError {
    Cancelled(QueryCancelled),
    MemoryLimit { required: usize, limit: usize },
    Allocation(TryReserveError),
    Memory(uqa_core::memory::MemoryError),
    Storage(StorageBackendError),
    Scoring(TextSearchError),
    InvalidGraph(String),
}

pub type PhraseResult<T> = Result<T, PhraseError>;

impl fmt::Display for PhraseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled(error) => error.fmt(f),
            Self::MemoryLimit { required, limit } => write!(
                f,
                "phrase execution requires {required} bytes, exceeding work_mem of {limit} bytes"
            ),
            Self::Allocation(error) => error.fmt(f),
            Self::Memory(error) => error.fmt(f),
            Self::Storage(error) => error.fmt(f),
            Self::Scoring(error) => error.fmt(f),
            Self::InvalidGraph(message) => f.write_str(message),
        }
    }
}

impl Error for PhraseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cancelled(error) => Some(error),
            Self::Allocation(error) => Some(error),
            Self::Memory(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Scoring(error) => Some(error),
            Self::MemoryLimit { .. } | Self::InvalidGraph(_) => None,
        }
    }
}

impl From<StorageBackendError> for PhraseError {
    fn from(error: StorageBackendError) -> Self {
        match error {
            StorageBackendError::Memory(error) => error.into(),
            StorageBackendError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Storage(error),
        }
    }
}

impl From<TextSearchError> for PhraseError {
    fn from(error: TextSearchError) -> Self {
        match error {
            TextSearchError::Memory(error) => error.into(),
            TextSearchError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Scoring(error),
        }
    }
}

impl From<uqa_core::memory::MemoryError> for PhraseError {
    fn from(error: uqa_core::memory::MemoryError) -> Self {
        match error {
            uqa_core::memory::MemoryError::Limit { required, limit } => {
                Self::MemoryLimit { required, limit }
            }
            uqa_core::memory::MemoryError::Allocation(error) => Self::Allocation(error),
            error @ uqa_core::memory::MemoryError::SizeOverflow => Self::Memory(error),
        }
    }
}
