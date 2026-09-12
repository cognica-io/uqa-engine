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
            Self::Storage(error) => Some(error),
            Self::Scoring(error) => Some(error),
            Self::MemoryLimit { .. } | Self::InvalidGraph(_) => None,
        }
    }
}

impl From<StorageBackendError> for PhraseError {
    fn from(error: StorageBackendError) -> Self {
        Self::Storage(error)
    }
}

impl From<TextSearchError> for PhraseError {
    fn from(error: TextSearchError) -> Self {
        Self::Scoring(error)
    }
}
