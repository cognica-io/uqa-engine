//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identities selected by parse analysis, before executable-plan dependencies are introduced.

use crate::{ast::FunctionBinding, ColumnType, ScalarExpr};
use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};
use uqa_core::Value;

/// Relations and selected routines used by the analyzed statement. Domain constraint dependencies introduced by optimization do not invalidate the statement's already-read input constants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedAnalysisDependencies {
    pub relations: BTreeSet<u32>,
    pub routines: BTreeSet<[u8; 16]>,
}

/// An exact provider-owned revision whose representation and retained resources stay opaque to SQL. Equality invokes the original type's equality; no serialization, hash or pointer-to-integer conversion discards its identity lifetime.
#[derive(Clone)]
pub struct PreparedDependencyRevision(Arc<dyn RevisionEquality>);

trait RevisionEquality: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn equals(&self, other: &dyn RevisionEquality) -> bool;
}

impl<T: Eq + Send + Sync + 'static> RevisionEquality for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn RevisionEquality) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
}

impl PreparedDependencyRevision {
    #[must_use]
    pub fn new<T: Eq + Send + Sync + 'static>(revision: T) -> Self {
        Self(Arc::new(revision))
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
    pub relations: BTreeMap<u32, Option<PreparedDependencyRevision>>,
    pub routines: BTreeMap<[u8; 16], Option<PreparedDependencyRevision>>,
}

impl PreparedAnalysisDependencies {
    pub(crate) fn include_routine(&mut self, binding: &FunctionBinding) {
        if !binding.builtin {
            if let Some(identity) = binding.object_id {
                self.routines.insert(identity);
            }
        }
    }

    /// `PostgreSQL`'s `ISREGCLASSCONST` includes non-null scalar regclass and oid constants. A whole array constant contributes no scalar relation identity; an array expression's scalar children do.
    pub(crate) fn include_expression(&mut self, expression: &ScalarExpr) {
        expression.visit(&mut |expression| match expression {
            ScalarExpr::TypedLiteral {
                value: Value::Int(oid),
                ty,
                bound_type,
                parameter_index: None,
                ..
            } => {
                let parsed;
                let ty = if let Some(ty) = bound_type.as_ref() {
                    Some(ty)
                } else {
                    parsed = ColumnType::from_sql_name(ty).ok();
                    parsed.as_ref()
                };
                if matches!(ty, Some(ColumnType::Regclass | ColumnType::Oid)) {
                    if let Ok(oid) = u32::try_from(*oid) {
                        self.relations.insert(oid);
                    }
                }
            }
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            } => self.include_routine(binding),
            _ => {}
        });
    }
}

#[cfg(test)]
mod tests;
