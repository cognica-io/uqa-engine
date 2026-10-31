//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identities selected by parse analysis, before executable-plan dependencies are introduced.

use crate::ScalarExpr;
use std::{any::Any, fmt, sync::Arc};
use uqa_core::memory::{MemoryReservation, OwnedMap, OwnedSet, Produced, ProductionControl};

mod production;

/// Relations and selected routines used by the analyzed statement. Domain constraint dependencies introduced by optimization do not invalidate the statement's already-read input constants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedAnalysisDependencies {
    pub relations: OwnedSet<u32>,
    pub routines: OwnedSet<[u8; 16]>,
}

/// An exact provider-owned revision whose representation and retained resources stay opaque to SQL. Equality invokes the original type's equality; no serialization, hash or pointer-to-integer conversion discards its identity lifetime.
#[derive(Clone)]
pub struct PreparedDependencyRevision(Arc<dyn RevisionEquality>);

trait RevisionEquality: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn equals(&self, other: &dyn RevisionEquality) -> bool;
}

struct Revision<T> {
    value: T,
    _memory: Option<MemoryReservation>,
}

impl<T: Eq + Send + Sync + 'static> RevisionEquality for Revision<T> {
    fn as_any(&self) -> &dyn Any {
        &self.value
    }

    fn equals(&self, other: &dyn RevisionEquality) -> bool {
        other.as_any().downcast_ref::<T>() == Some(&self.value)
    }
}

impl PreparedDependencyRevision {
    #[must_use]
    pub fn new<T: Eq + Send + Sync + 'static>(revision: T) -> Self {
        let control = ProductionControl::uncontrolled();
        Self::from_produced(
            control.finish(revision, None).expect("ordinary revision"),
            &control,
        )
        .expect("ordinary revision publication")
    }

    pub fn from_produced<T: Eq + Send + Sync + 'static>(
        revision: Produced<T>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        let header = control.reserve(size_of::<Revision<T>>())?;
        let (value, memory) = revision.into_parts();
        Ok(Self(Arc::new(Revision {
            value,
            _memory: control.combine(memory, header),
        })))
    }
}

impl PartialEq for PreparedDependencyRevision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.equals(other.0.as_ref())
    }
}

impl Eq for PreparedDependencyRevision {}

impl fmt::Debug for PreparedDependencyRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedDependencyRevision")
            .finish_non_exhaustive()
    }
}

/// Revisions read from the same immutable catalog scope that resolved the identities. A missing map value represents an object that no longer exists, rather than an unsupported snapshot capability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedDependencySnapshot {
    /// Global parse-analysis invalidations, such as replacement of a namespace catalog tuple.
    pub global_catalog: Option<PreparedDependencyRevision>,
    pub relations: OwnedMap<u32, Option<PreparedDependencyRevision>>,
    pub routines: OwnedMap<[u8; 16], Option<PreparedDependencyRevision>>,
}

impl PreparedDependencySnapshot {
    /// Copy only selected map nodes; opaque revision payloads keep their original shared owners.
    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        let mut output = Self {
            global_catalog: self.global_catalog.clone(),
            ..Self::default()
        };
        for (key, value) in &self.relations {
            control.check()?;
            match control.budget() {
                Some(budget) => {
                    output
                        .relations
                        .insert_budgeted(*key, value.clone(), budget)?;
                }
                None => {
                    output.relations.insert(*key, value.clone());
                }
            }
        }
        for (key, value) in &self.routines {
            control.check()?;
            match control.budget() {
                Some(budget) => {
                    output
                        .routines
                        .insert_budgeted(*key, value.clone(), budget)?;
                }
                None => {
                    output.routines.insert(*key, value.clone());
                }
            }
        }
        Ok(output)
    }
}

impl PreparedAnalysisDependencies {
    /// Scalar regclass and OID constants retain relation identity; an array constant does not contribute its elements.
    pub(crate) fn include_expression(&mut self, expression: &ScalarExpr) {
        self.include_expression_with_control(expression, &ProductionControl::uncontrolled())
            .expect("ordinary expression dependency");
    }
}

#[cfg(test)]
mod tests;
