//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE lifetime, visibility, and scalar-subquery arena scopes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use uqa_planner::{CtePlan, QueryPlan};

use super::{CteScope, LockIdentityOptions};

impl CteScope {
    pub(in crate::sql) fn insert_shared(&mut self, name: String, rows: uqa_execution::SharedSpill) {
        self.deferred_ctes.remove(&name);
        self.rows.insert(name, rows);
    }

    pub(in crate::sql) fn insert_deferred(&mut self, plan: CtePlan) {
        self.rows.remove(&plan.name);
        self.deferred_ctes.insert(plan.name.clone(), plan);
    }

    pub(in crate::sql) fn remove_deferred(&mut self, name: &str) -> Option<CtePlan> {
        self.deferred_ctes.remove(name)
    }

    /// Return one deferred CTE for a scan. `NOT MATERIALIZED` definitions remain available so every syntactic reference is independently folded, while the default single-reference fast path is consumed exactly once.
    pub(in crate::sql) fn deferred_for_scan(&mut self, name: &str) -> Option<CtePlan> {
        let persistent = self.deferred_ctes.get(name).is_some_and(|plan| {
            plan.materialization == uqa_sql::ast::CteMaterialization::NotMaterialized
        });
        if persistent {
            self.deferred_ctes.get(name).cloned()
        } else {
            self.deferred_ctes.remove(name)
        }
    }

    pub(in crate::sql) fn deferred_ctes(&self) -> &BTreeMap<String, CtePlan> {
        &self.deferred_ctes
    }

    pub(in crate::sql) fn recursive_control_width(&self, name: &str) -> Option<usize> {
        self.recursive_control_widths.get(name).copied()
    }

    pub(in crate::sql) fn set_recursive_control_width(
        &mut self,
        name: String,
        width: usize,
    ) -> Option<usize> {
        self.recursive_control_widths.insert(name, width)
    }

    pub(in crate::sql) fn restore_recursive_control_width(
        &mut self,
        name: &str,
        previous: Option<usize>,
    ) {
        match previous {
            Some(width) => {
                self.recursive_control_widths
                    .insert(name.to_string(), width);
            }
            None => {
                self.recursive_control_widths.remove(name);
            }
        }
    }

    pub(in crate::sql) fn remove_materialized(
        &mut self,
        name: &str,
    ) -> Option<uqa_execution::SharedSpill> {
        self.rows.remove(name)
    }

    /// Bind the scalar-subquery arena owned by one query block. The guard restores the parent arena on success, error, or panic so nested and lateral query execution cannot resolve a child slot in its parent.
    pub(in crate::sql) fn enter_scalar_subqueries(
        &mut self,
        subqueries: &[QueryPlan],
    ) -> ScalarSubqueryScope<'_> {
        let previous = std::mem::replace(&mut self.scalar_subqueries, subqueries.to_vec());
        let next_arena = self
            .next_scalar_subquery_arena
            .fetch_add(1, Ordering::Relaxed);
        let previous_arena = std::mem::replace(&mut self.scalar_subquery_arena, next_arena);
        let previous_lock_identities = self.lock_identities;
        ScalarSubqueryScope {
            ctes: self,
            previous: Some(previous),
            previous_arena,
            previous_lock_identities,
        }
    }

    pub(in crate::sql) fn enter_visible_ctes<'a>(
        &'a mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> VisibleCteScope<'a> {
        let previous = std::mem::take(&mut self.visible_cte_names);
        self.visible_cte_names.clone_from(&previous);
        self.visible_cte_names
            .extend(names.into_iter().map(str::to_owned));
        VisibleCteScope {
            ctes: self,
            previous: Some(previous),
        }
    }

    /// Whether `name` resolves to a CTE in this scope: a name declared by an enclosing query's WITH list, or one whose rows or deferred plan are bound in the scope, which is how a DML statement's own WITH list reaches the query it drives.
    pub(in crate::sql) fn is_visible_cte(&self, name: &str) -> bool {
        self.visible_cte_names.contains(name)
            || self.rows.contains_key(name)
            || self.deferred_ctes.contains_key(name)
    }

    pub(in crate::sql) fn returning_statement_snapshot_scope(&self) -> Self {
        let mut scope = self.clone();
        scope.read_command_overlay = false;
        scope
    }

    pub(in crate::sql) fn reads_command_overlay(&self) -> bool {
        self.read_command_overlay
    }

    pub(in crate::sql) fn enable_command_progress_streaming(&mut self) {
        self.stream_command_progress = true;
    }

    pub(in crate::sql) fn streams_command_progress(&self) -> bool {
        self.stream_command_progress
    }

    pub(in crate::sql) fn enable_backwards_scanning(&mut self) {
        self.scan_backwards = true;
    }

    pub(in crate::sql) fn scans_backwards(&self) -> bool {
        self.scan_backwards
    }
}

pub(in crate::sql) struct ScalarSubqueryScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<QueryPlan>>,
    previous_arena: u64,
    previous_lock_identities: LockIdentityOptions,
}

impl std::ops::Deref for ScalarSubqueryScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for ScalarSubqueryScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for ScalarSubqueryScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.scalar_subqueries = previous;
            self.ctes.scalar_subquery_arena = self.previous_arena;
            self.ctes.lock_identities = self.previous_lock_identities;
        }
    }
}

pub(in crate::sql) struct VisibleCteScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<BTreeSet<String>>,
}

impl std::ops::Deref for VisibleCteScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for VisibleCteScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for VisibleCteScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.visible_cte_names = previous;
        }
    }
}
