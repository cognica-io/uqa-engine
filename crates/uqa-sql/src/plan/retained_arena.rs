//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable scalar-query namespaces keep their constructor leases through shared execution scopes.

use super::{QueryPlan, ScalarQueryArena};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, Clone, Default)]
pub struct RetainedQueryArena(Storage);

#[derive(Debug, Clone, Default)]
enum Storage {
    #[default]
    Empty,
    Ordinary(Arc<[QueryPlan]>),
    Admitted(Arc<Produced<Vec<QueryPlan>>>),
}

impl RetainedQueryArena {
    /// Admit a complete private arena before copying any query payload. Clones of the returned owner share that arena without new allocation.
    pub fn capture(
        queries: &[QueryPlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if queries.is_empty() {
            return Ok(Self::default());
        }
        ScalarQueryArena::copy_from(queries, control)?.into_shared(control)
    }

    fn from_produced(
        queries: Produced<Vec<QueryPlan>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if queries.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self(Storage::Admitted(queries.into_shared(control)?)))
    }

    pub fn reserved_bytes(&self) -> usize {
        match &self.0 {
            Storage::Admitted(queries) => queries.reserved_bytes(),
            Storage::Empty | Storage::Ordinary(_) => 0,
        }
    }

    /// Share an admitted arena with another reader, or admit a legacy caller's ordinary input before retaining its private copy.
    pub fn retain_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        match self.0 {
            Storage::Ordinary(_) => Self::capture(self, control),
            Storage::Empty | Storage::Admitted(_) => Ok(self.clone()),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Storage::Empty, Storage::Empty) => true,
            (Storage::Ordinary(left), Storage::Ordinary(right)) => Arc::ptr_eq(left, right),
            (Storage::Admitted(left), Storage::Admitted(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl ScalarQueryArena {
    /// Transfer the arena buffer and its existing payload ownership into shared storage without copying a query or changing its private slot identity.
    pub fn into_shared(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<RetainedQueryArena, SQLError> {
        RetainedQueryArena::from_produced(self.finish(control)?, control)
    }
}

impl AsRef<[QueryPlan]> for RetainedQueryArena {
    fn as_ref(&self) -> &[QueryPlan] {
        match &self.0 {
            Storage::Empty => &[],
            Storage::Ordinary(queries) => queries,
            Storage::Admitted(queries) => queries,
        }
    }
}

impl std::ops::Deref for RetainedQueryArena {
    type Target = [QueryPlan];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<'a> IntoIterator for &'a RetainedQueryArena {
    type Item = &'a QueryPlan;
    type IntoIter = std::slice::Iter<'a, QueryPlan>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl From<Vec<QueryPlan>> for RetainedQueryArena {
    fn from(queries: Vec<QueryPlan>) -> Self {
        if queries.is_empty() {
            Self::default()
        } else {
            Self(Storage::Ordinary(queries.into()))
        }
    }
}

impl From<Arc<[QueryPlan]>> for RetainedQueryArena {
    fn from(queries: Arc<[QueryPlan]>) -> Self {
        Self(Storage::Ordinary(queries))
    }
}

impl From<&[QueryPlan]> for RetainedQueryArena {
    fn from(queries: &[QueryPlan]) -> Self {
        if queries.is_empty() {
            Self::default()
        } else {
            Self(Storage::Ordinary(queries.into()))
        }
    }
}

#[cfg(test)]
mod tests;
