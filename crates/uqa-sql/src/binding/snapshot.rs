//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned catalog and CTE type inputs for analysis that rewrites nested scopes.

use super::context::BindingContext;
use crate::{
    catalog::{analysis::CatalogReadView, resolution::RelationNameResolution},
    plan::{CtePlan, QueryPlan},
    RowSchema,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct BindingSnapshot {
    pub catalog: CatalogReadView,
    pub resolution: RelationNameResolution,
    pub ctes: BTreeMap<String, RowSchema>,
    pub deferred_ctes: BTreeMap<String, CtePlan>,
    pub non_returning_ctes: BTreeSet<String>,
    pub scalar_subqueries: Vec<QueryPlan>,
}
impl From<BindingContext<'_>> for BindingSnapshot {
    fn from(context: BindingContext<'_>) -> Self {
        Self {
            catalog: context.catalog,
            resolution: context.resolution,
            ctes: context.ctes,
            deferred_ctes: context.deferred_ctes,
            non_returning_ctes: context.non_returning_ctes,
            scalar_subqueries: context.scalar_subqueries.to_vec(),
        }
    }
}
impl BindingSnapshot {
    pub fn context(&self) -> BindingContext<'_> {
        BindingContext {
            catalog: self.catalog.clone(),
            resolution: self.resolution.clone(),
            ctes: self.ctes.clone(),
            deferred_ctes: self.deferred_ctes.clone(),
            non_returning_ctes: self.non_returning_ctes.clone(),
            scalar_subqueries: &self.scalar_subqueries,
        }
    }
    pub fn inherit_cte_bindings(&mut self, parent: &Self) {
        self.ctes.clone_from(&parent.ctes);
        self.deferred_ctes.clone_from(&parent.deferred_ctes);
        self.non_returning_ctes
            .clone_from(&parent.non_returning_ctes);
    }
    pub fn insert_deferred(&mut self, plan: CtePlan) {
        self.ctes.remove(&plan.name);
        if plan.body.returns_rows() {
            self.non_returning_ctes.remove(&plan.name);
        } else {
            self.non_returning_ctes.insert(plan.name.clone());
        }
        self.deferred_ctes.insert(plan.name.clone(), plan);
    }
}
