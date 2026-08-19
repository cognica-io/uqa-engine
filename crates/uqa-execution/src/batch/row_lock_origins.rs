//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock lineage carried on composite physical rows.

use std::sync::Arc;

use super::PhysicalRow;

/// Base-table identity carried on a composite physical row for `FOR UPDATE`.
///
/// Origins ride beside value fragments. Joins concatenate them; projections and schema remaps keep them. They are not SQL-visible columns and must not be rebuilt from named maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLockOrigin {
    /// Visible qualifier of the row source that currently owns the origin. Views, CTEs, and derived tables rebind it to their own alias.
    pub qualifier: Arc<str>,
    /// Qualifier of the base scan that produced the origin. Rebinding at a derived-table boundary leaves it untouched, so a tuple recheck can pin each base scan inside a view or subquery to its own tuples.
    pub scan_qualifier: Arc<str>,
    pub storage_name: Arc<str>,
    pub doc_id: uqa_core::DocId,
}

impl RowLockOrigin {
    #[must_use]
    pub fn new(
        qualifier: impl Into<String>,
        storage_name: impl Into<String>,
        doc_id: uqa_core::DocId,
    ) -> Self {
        Self::from_shared(
            Arc::<str>::from(qualifier.into()),
            Arc::<str>::from(storage_name.into()),
            doc_id,
        )
    }

    /// Build an origin from source names shared by every row in one scan.
    #[must_use]
    pub fn from_shared(
        qualifier: Arc<str>,
        storage_name: Arc<str>,
        doc_id: uqa_core::DocId,
    ) -> Self {
        Self {
            scan_qualifier: Arc::clone(&qualifier),
            qualifier,
            storage_name,
            doc_id,
        }
    }
}

pub(super) fn concat_lock_origins(
    left: Option<&Arc<[RowLockOrigin]>>,
    right: Option<&Arc<[RowLockOrigin]>>,
) -> Option<Arc<[RowLockOrigin]>> {
    match (left, right) {
        (None, None) => None,
        (Some(origins), None) | (None, Some(origins)) => Some(Arc::clone(origins)),
        (Some(left), Some(right)) => {
            let mut origins = Vec::with_capacity(left.len() + right.len());
            origins.extend(left.iter().cloned());
            origins.extend(right.iter().cloned());
            Some(Arc::from(origins))
        }
    }
}

impl PhysicalRow {
    #[must_use]
    pub fn with_lock_origin(mut self, origin: RowLockOrigin) -> Self {
        self.lock_origins = match self.lock_origins.take() {
            None => Some(Arc::from([origin])),
            Some(existing) => {
                let mut origins = Vec::with_capacity(existing.len() + 1);
                origins.extend(existing.iter().cloned());
                origins.push(origin);
                Some(Arc::from(origins))
            }
        };
        self
    }

    #[must_use]
    pub fn with_lock_origins(mut self, origins: impl IntoIterator<Item = RowLockOrigin>) -> Self {
        let origins = origins.into_iter().collect::<Vec<_>>();
        if origins.is_empty() {
            return self;
        }
        self.lock_origins = match self.lock_origins.take() {
            None => Some(Arc::from(origins)),
            Some(existing) => {
                let mut combined = Vec::with_capacity(existing.len() + origins.len());
                combined.extend(existing.iter().cloned());
                combined.extend(origins);
                Some(Arc::from(combined))
            }
        };
        self
    }

    #[must_use]
    pub fn lock_origins(&self) -> &[RowLockOrigin] {
        self.lock_origins.as_deref().unwrap_or(&[])
    }

    /// Drop row-lock lineage at an execution boundary that cannot expose a lockable base-row identity, such as a set operation.
    #[must_use]
    pub fn without_lock_origins(mut self) -> Self {
        self.lock_origins = None;
        self
    }

    /// Remove all row-lock identities without reallocating the row payload.
    pub fn discard_lock_origins_mut(&mut self) {
        self.lock_origins = None;
    }

    /// Point every lock origin at the visible source qualifier. Views, CTEs, and subqueries keep inner storage names so `FOR UPDATE OF` that alias locks only those origins after a join.
    #[must_use]
    pub fn rebind_lock_origin_qualifiers(mut self, qualifier: impl Into<Arc<str>>) -> Self {
        self.rebind_lock_origin_qualifiers_mut(qualifier.into());
        self
    }

    pub fn rebind_lock_origin_qualifiers_mut(&mut self, qualifier: Arc<str>) {
        let Some(origins) = self.lock_origins.as_mut() else {
            return;
        };
        for origin in Arc::make_mut(origins) {
            origin.qualifier = Arc::clone(&qualifier);
        }
    }
}
