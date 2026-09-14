//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable source buffers share their reservation with every retained view.

use std::sync::Arc;
use uqa_core::memory::Budgeted;

#[derive(Debug, Clone)]
pub(super) enum SourceText<'a> {
    Borrowed(&'a str),
    Owned(Arc<Budgeted<String>>),
}

impl SourceText<'_> {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Owned(text) => text,
        }
    }

    pub fn into_string(self) -> String {
        match self {
            Self::Borrowed(text) => text.to_owned(),
            Self::Owned(text) => match Arc::try_unwrap(text) {
                Ok(text) => text.into_parts().0,
                Err(text) => text.as_str().to_owned(),
            },
        }
    }
}
