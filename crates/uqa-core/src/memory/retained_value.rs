//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared immutable values retain admitted allocations through ordinary copy-on-write successors.

use super::{Budgeted, MemoryBudget, MemoryError};
use std::{
    ops::Deref,
    sync::{Arc, Weak},
};

/// An immutable shared native value with an optional admitted header. Payloads retain their existing native ownership; a mutable successor preserves that ownership through the native Clone operation.
pub struct RetainedValue<T>(Owner<T>);

pub(crate) enum Owner<T> {
    Native(Arc<T>),
    Admitted(Arc<Budgeted<Admitted<T>>>),
}

#[derive(Debug)]
pub(crate) struct Admitted<T> {
    pub(crate) payload: T,
    identity: Arc<()>,
}

/// A non-owning identity for cache invalidation. It cannot read a value or keep an admitted payload alive. The separate zero-sized identity prevents allocation-address reuse from making a different value appear current.
pub struct RetainedValueIdentity<T>(Identity<T>);

enum Identity<T> {
    Native(Weak<T>),
    Admitted(Weak<()>),
}

impl<T> Clone for RetainedValue<T> {
    fn clone(&self) -> Self {
        match &self.0 {
            Owner::Native(value) => Self(Owner::Native(value.clone())),
            Owner::Admitted(value) => Self(Owner::Admitted(value.clone())),
        }
    }
}

impl<T> From<Arc<T>> for RetainedValue<T> {
    fn from(value: Arc<T>) -> Self {
        Self(Owner::Native(value))
    }
}

impl<T> Deref for RetainedValue<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match &self.0 {
            Owner::Native(value) => value,
            Owner::Admitted(value) => &value.payload,
        }
    }
}

impl<T> AsRef<T> for RetainedValue<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T> RetainedValue<T> {
    pub fn new(value: T) -> Self {
        Self(Owner::Native(Arc::new(value)))
    }

    /// Admit the shared header before allocating it. The native value owns its payloads independently; this constructor never adopts a separate payload reservation whose lifetime could be lost by Clone.
    pub fn with_budget(value: T, budget: &MemoryBudget) -> Result<Self, MemoryError> {
        let memory = budget.reserve(size_of::<Budgeted<Admitted<T>>>())?;
        let value = Admitted {
            payload: value,
            identity: Arc::new(()),
        };
        Ok(Self(Owner::Admitted(Arc::new(Budgeted::new(
            value, memory,
        )))))
    }

    /// Compare the original shared allocation, independently of equal payloads.
    pub fn ptr_eq(left: &Self, right: &Self) -> bool {
        match (&left.0, &right.0) {
            (Owner::Native(left), Owner::Native(right)) => Arc::ptr_eq(left, right),
            (Owner::Admitted(left), Owner::Admitted(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub fn identity(&self) -> RetainedValueIdentity<T> {
        RetainedValueIdentity(match &self.0 {
            Owner::Native(value) => Identity::Native(Arc::downgrade(value)),
            Owner::Admitted(value) => Identity::Admitted(Arc::downgrade(&value.identity)),
        })
    }

    pub(crate) fn into_owner(self) -> Owner<T> {
        self.0
    }
}

impl<T: Clone> RetainedValue<T> {
    /// Detach a mutable successor while surviving readers retain the original header. Payload ownership follows the native Clone operation. A native unique allocation retains ordinary Arc mutation semantics.
    pub fn make_mut(value: &mut Self) -> &mut T {
        if let Owner::Admitted(original) = &value.0 {
            value.0 = Owner::Native(Arc::new(original.payload.clone()));
        }
        let Owner::Native(value) = &mut value.0 else {
            unreachable!("the successor owns a native mutable generation")
        };
        Arc::make_mut(value)
    }
}

impl<T: Default> Default for RetainedValue<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for RetainedValue<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(formatter)
    }
}

impl<T: Eq> PartialEq for RetainedValue<T> {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other) || self.as_ref() == other.as_ref()
    }
}

impl<T: Eq> Eq for RetainedValue<T> {}

impl<T> RetainedValueIdentity<T> {
    /// Observe whether a strong value owner still exists. This observation does not retain or grant access to that value.
    pub fn is_alive(&self) -> bool {
        match &self.0 {
            Identity::Native(identity) => identity.strong_count() != 0,
            Identity::Admitted(identity) => identity.strong_count() != 0,
        }
    }

    pub fn matches(&self, value: &RetainedValue<T>) -> bool {
        match (&self.0, &value.0) {
            (Identity::Native(identity), Owner::Native(value)) => {
                Weak::ptr_eq(identity, &Arc::downgrade(value))
            }
            (Identity::Admitted(identity), Owner::Admitted(value)) => {
                Weak::ptr_eq(identity, &Arc::downgrade(&value.identity))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
