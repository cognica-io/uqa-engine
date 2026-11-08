//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared native values keep their original image allowance through every reader.

use std::{ops::Deref, sync::Arc};

use uqa_core::{memory::Budgeted, Value};

/// A native positional row and, when decoded under an allowance, the lease of its original allocations. Both variants expose the same values and remain immutable while shared.
#[derive(Debug, Clone)]
pub enum SharedDocumentValues {
    Ordinary(Arc<Vec<Value>>),
    Retained(Arc<Budgeted<Vec<Value>>>),
}

impl SharedDocumentValues {
    pub fn is_shared(&self) -> bool {
        match self {
            Self::Ordinary(values) => Arc::strong_count(values) > 1,
            Self::Retained(values) => Arc::strong_count(values) > 1,
        }
    }

    /// Identify the live native vector for grouping aliases. This address is never a catalog or persisted identity.
    pub fn as_ptr(&self) -> *const Vec<Value> {
        match self {
            Self::Ordinary(values) => Arc::as_ptr(values),
            Self::Retained(values) => std::ptr::from_ref(&***values),
        }
    }
}

impl Deref for SharedDocumentValues {
    type Target = Vec<Value>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ordinary(values) => values,
            Self::Retained(values) => values,
        }
    }
}

impl PartialEq for SharedDocumentValues {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

/// A thin shared projection handle. Native fragments keep their inline footprint, while legacy readers share the exact original slot allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedDocumentProjection(Arc<Arc<[usize]>>);

impl SharedDocumentProjection {
    pub fn new(slots: impl Into<Arc<[usize]>>) -> Self {
        Self(Arc::new(slots.into()))
    }

    /// Retain the original slot allocation without copying its contents.
    pub fn shared_slots(&self) -> Arc<[usize]> {
        Arc::clone(&self.0)
    }
}

impl Deref for SharedDocumentProjection {
    type Target = [usize];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
