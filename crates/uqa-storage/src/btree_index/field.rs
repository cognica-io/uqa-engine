//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable imported field names share their original allocation owner.

use std::{ops::Deref, sync::Arc};
use uqa_core::memory::Budgeted;

#[derive(Debug, Clone)]
pub(super) enum IndexField {
    Native(String),
    Admitted(Arc<Budgeted<String>>),
}

impl Default for IndexField {
    fn default() -> Self {
        Self::Native(String::new())
    }
}

impl Deref for IndexField {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Native(name) => name,
            Self::Admitted(name) => name,
        }
    }
}
