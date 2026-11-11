//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{memory::MemoryError, QueryCancelled, ValueRetentionError};

/// Representation errors need no heap diagnostic while an optional flattened field is still being selected. Resource failures never become absent metadata.
#[derive(Debug)]
pub(crate) enum InputError {
    Invalid,
    Resource(ValueRetentionError),
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid stored SQL declaration JSON"),
            Self::Resource(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for InputError {}

impl serde::de::Error for InputError {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self::Invalid
    }
}

impl From<ValueRetentionError> for InputError {
    fn from(error: ValueRetentionError) -> Self {
        Self::Resource(error)
    }
}

impl From<MemoryError> for InputError {
    fn from(error: MemoryError) -> Self {
        Self::Resource(error.into())
    }
}

impl From<QueryCancelled> for InputError {
    fn from(error: QueryCancelled) -> Self {
        Self::Resource(error.into())
    }
}

impl From<InputError> for crate::SQLError {
    fn from(error: InputError) -> Self {
        match error {
            InputError::Invalid => Self::Internal("invalid stored SQL declaration JSON".into()),
            InputError::Resource(error) => error.into(),
        }
    }
}
