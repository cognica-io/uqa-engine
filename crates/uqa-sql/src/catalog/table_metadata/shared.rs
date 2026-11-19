//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata consumers retain the same allocation owner as their selected generation.

use super::{footprint, TableMetadataValue};
use std::{ops::Deref, sync::Arc};
use uqa_core::memory::Budgeted;

#[derive(Debug, Clone)]
enum Owner<T: TableMetadataValue> {
    Native(Arc<T>),
    Admitted(Arc<Budgeted<T>>),
}

/// Immutable metadata and its complete native ownership. Imported reservations remain live until the final consumer releases its selected generation.
#[derive(Debug, Clone)]
pub struct SharedTableMetadata<T: TableMetadataValue>(Owner<T>);

impl<T: TableMetadataValue> SharedTableMetadata<T> {
    pub fn shares_definition(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Owner::Native(left), Owner::Native(right)) => Arc::ptr_eq(left, right),
            (Owner::Admitted(left), Owner::Admitted(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub(super) fn admitted(value: Arc<Budgeted<T>>) -> Self {
        Self(Owner::Admitted(value))
    }

    pub(super) fn retained_bytes(&self) -> Option<usize> {
        match &self.0 {
            Owner::Native(value) => footprint::retained(&**value),
            Owner::Admitted(value) => footprint::admitted(value),
        }
    }

    /// Detach this consumer before mutation. Other consumers retain the original value and admission; the new native value has its own allocation lifetime.
    pub fn make_mut(&mut self) -> &mut T {
        if let Owner::Admitted(value) = &self.0 {
            self.0 = Owner::Native(Arc::new((***value).clone()));
        }
        match &mut self.0 {
            Owner::Native(value) => Arc::make_mut(value),
            Owner::Admitted(_) => unreachable!("detached metadata generation"),
        }
    }
}

impl<T: TableMetadataValue> From<Arc<T>> for SharedTableMetadata<T> {
    fn from(value: Arc<T>) -> Self {
        Self(Owner::Native(value))
    }
}

impl<T: TableMetadataValue> From<T> for SharedTableMetadata<T> {
    fn from(value: T) -> Self {
        Arc::new(value).into()
    }
}

impl<T: TableMetadataValue + Default> Default for SharedTableMetadata<T> {
    fn default() -> Self {
        T::default().into()
    }
}

impl<T: TableMetadataValue> Deref for SharedTableMetadata<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.0 {
            Owner::Native(value) => value,
            Owner::Admitted(value) => value,
        }
    }
}

impl<T: TableMetadataValue> AsRef<T> for SharedTableMetadata<T> {
    fn as_ref(&self) -> &T {
        self
    }
}
