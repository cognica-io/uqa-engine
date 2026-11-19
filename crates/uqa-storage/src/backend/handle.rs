//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A writable native handle retires its boxed allocation before returning its allowance.

use std::ops::{Deref, DerefMut};
use uqa_core::memory::{Budgeted, MemoryBudget};

/// A native boxed storage object with optional original allocation ownership. Mutable access exposes the object, so replacing the entire handle also retires its original box reservation.
pub struct StorageHandle<T: ?Sized>(Owner<T>);

enum Owner<T: ?Sized> {
    Native(Box<T>),
    Admitted(Budgeted<Box<T>>),
}

impl<T: ?Sized> StorageHandle<T> {
    pub fn new(value: Box<T>) -> Self {
        Self(Owner::Native(value))
    }

    /// Construction admission must retain the complete concrete box under the original allowance.
    pub(crate) fn is_retained_by(&self, memory: &MemoryBudget) -> bool {
        match &self.0 {
            Owner::Native(_) => false,
            Owner::Admitted(value) => {
                value.budget().shares_allowance(memory)
                    && value.reserved_bytes() >= size_of_val(&***value)
            }
        }
    }
}

impl<T: ?Sized> From<Budgeted<Box<T>>> for StorageHandle<T> {
    fn from(value: Budgeted<Box<T>>) -> Self {
        Self(Owner::Admitted(value))
    }
}

impl<T: ?Sized> From<Box<T>> for StorageHandle<T> {
    fn from(value: Box<T>) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized> Deref for StorageHandle<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.0 {
            Owner::Native(value) => value,
            Owner::Admitted(value) => value,
        }
    }
}

impl<T: ?Sized> DerefMut for StorageHandle<T> {
    fn deref_mut(&mut self) -> &mut T {
        match &mut self.0 {
            Owner::Native(value) => value,
            Owner::Admitted(value) => value.as_mut(),
        }
    }
}

impl<T: ?Sized> AsRef<T> for StorageHandle<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized> AsMut<T> for StorageHandle<T> {
    fn as_mut(&mut self) -> &mut T {
        self
    }
}
