//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE lifetime, visibility, and scalar-subquery arena scopes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use uqa_sql::plan::{CtePlan, QueryPlan};
use uqa_sql::SQLError;

use super::{CteScope, LockIdentityOptions};
use uqa_sql::catalog::resolution::RelationLookupMode;

impl<S: Clone> CteScope<S> {
    pub fn command_cte_snapshot(&self) -> Option<std::sync::Arc<S>> {
        self.command_cte_snapshot.clone()
    }

    pub fn set_command_cte_snapshot(&mut self, snapshot: Option<std::sync::Arc<S>>) {
        self.command_cte_snapshot = snapshot;
    }

    pub fn inherit_cte_bindings(&mut self, parent: &Self) {
        self.rows.clone_from(&parent.rows);
        self.deferred_ctes.clone_from(&parent.deferred_ctes);
        self.non_returning_ctes
            .clone_from(&parent.non_returning_ctes);
        self.visible_cte_names.clone_from(&parent.visible_cte_names);
        self.recursive_control_widths
            .clone_from(&parent.recursive_control_widths);
        self.command_cte_snapshot
            .clone_from(&parent.command_cte_snapshot);
        if self.privilege_subject.is_none() {
            self.privilege_subject.clone_from(&parent.privilege_subject);
        }
    }

    /// Override only relation privilege checks while preserving SQL-visible `current_user` and the caller's namespace.
    pub fn enter_privilege_subject(&mut self, subject: String) -> PrivilegeSubjectScope<'_, S> {
        let previous = self.privilege_subject.replace(subject);
        PrivilegeSubjectScope {
            ctes: self,
            previous,
        }
    }

    /// Select the namespace semantics owned by one query plan and restore the parent plan's mode on every exit path.
    pub fn enter_relation_lookup_mode(
        &mut self,
        relations_bound: bool,
    ) -> Result<RelationLookupScope<'_, S>, SQLError> {
        let resolution = self.catalog_resolution.as_mut().ok_or_else(|| {
            SQLError::Internal(
                "query execution scope has no statement name-resolution snapshot".into(),
            )
        })?;
        let lookup_mode = if relations_bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        };
        let previous = resolution.set_lookup_mode(lookup_mode);
        Ok(RelationLookupScope {
            ctes: self,
            previous,
        })
    }

    pub fn insert_shared(&mut self, name: String, rows: crate::SharedSpill) {
        self.deferred_ctes.remove(&name);
        self.non_returning_ctes.remove(&name);
        self.rows.insert(name, rows);
    }

    pub fn insert_deferred(&mut self, plan: CtePlan) {
        self.rows.remove(&plan.name);
        if plan.body.returns_rows() {
            self.non_returning_ctes.remove(&plan.name);
        } else {
            self.non_returning_ctes.insert(plan.name.clone());
        }
        self.deferred_ctes.insert(plan.name.clone(), plan);
    }

    pub fn remove_deferred(&mut self, name: &str) -> Option<CtePlan> {
        self.non_returning_ctes.remove(name);
        self.deferred_ctes.remove(name)
    }

    /// Return one deferred CTE for a scan. `NOT MATERIALIZED` definitions remain available so every syntactic reference is independently folded, while the default single-reference fast path is consumed exactly once.
    pub fn deferred_for_scan(&mut self, name: &str) -> Option<CtePlan> {
        let name = uqa_sql::semantics::cte_reference_name(name)?;
        let persistent = self.deferred_ctes.get(&name).is_some_and(|plan| {
            plan.materialization == uqa_sql::ast::CteMaterialization::NotMaterialized
        });
        if persistent {
            self.deferred_ctes.get(&name).cloned()
        } else {
            self.deferred_ctes.remove(&name)
        }
    }

    pub fn materialized_for_scan(&self, reference: &str) -> Option<crate::SharedSpill> {
        self.rows
            .get(&uqa_sql::semantics::cte_reference_name(reference)?)
            .cloned()
    }

    pub fn deferred_reference(&self, reference: &str) -> Option<&CtePlan> {
        self.deferred_ctes
            .get(&uqa_sql::semantics::cte_reference_name(reference)?)
    }

    pub fn deferred_ctes(&self) -> &BTreeMap<String, CtePlan> {
        &self.deferred_ctes
    }

    pub fn recursive_control_width(&self, name: &str) -> Option<usize> {
        self.recursive_control_widths.get(name).copied()
    }

    pub fn set_recursive_control_width(&mut self, name: String, width: usize) -> Option<usize> {
        self.recursive_control_widths.insert(name, width)
    }

    pub fn restore_recursive_control_width(&mut self, name: &str, previous: Option<usize>) {
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

    pub fn remove_materialized(&mut self, name: &str) -> Option<crate::SharedSpill> {
        self.non_returning_ctes.remove(name);
        self.rows.remove(name)
    }

    /// Bind the scalar-subquery arena owned by one query block. The guard restores the parent arena on success, error, or panic so nested and lateral query execution cannot resolve a child slot in its parent.
    pub fn enter_scalar_subqueries(
        &mut self,
        subqueries: &[QueryPlan],
    ) -> ScalarSubqueryScope<'_, S> {
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

    pub fn enter_visible_ctes<'a>(
        &'a mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> VisibleCteScope<'a, S> {
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
    pub fn is_visible_cte(&self, name: &str) -> bool {
        uqa_sql::semantics::cte_reference_name(name).is_some_and(|name| {
            self.visible_cte_names.contains(&name)
                || self.rows.contains_key(&name)
                || self.deferred_ctes.contains_key(&name)
        })
    }

    pub fn returning_statement_snapshot_scope(&self) -> Self {
        let mut scope = self.clone();
        scope.read_command_overlay = false;
        scope
    }

    pub fn reads_command_overlay(&self) -> bool {
        self.read_command_overlay
    }

    pub fn enable_command_progress_streaming(&mut self) {
        self.stream_command_progress = true;
    }

    pub fn streams_command_progress(&self) -> bool {
        self.stream_command_progress
    }

    pub fn enable_backwards_scanning(&mut self) {
        self.scan_backwards = true;
    }

    pub fn scans_backwards(&self) -> bool {
        self.scan_backwards
    }
}

pub struct PrivilegeSubjectScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: Option<String>,
}

impl<S: Clone> std::ops::Deref for PrivilegeSubjectScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for PrivilegeSubjectScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for PrivilegeSubjectScope<'_, S> {
    fn drop(&mut self) {
        self.ctes.privilege_subject = self.previous.take();
    }
}

pub struct RelationLookupScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: RelationLookupMode,
}

impl<S: Clone> std::ops::Deref for RelationLookupScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for RelationLookupScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for RelationLookupScope<'_, S> {
    fn drop(&mut self) {
        let resolution = self
            .ctes
            .catalog_resolution
            .as_mut()
            .expect("relation lookup scope lost its statement resolution");
        resolution.set_lookup_mode(self.previous);
    }
}

pub struct ScalarSubqueryScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: Option<Vec<QueryPlan>>,
    previous_arena: u64,
    previous_lock_identities: LockIdentityOptions,
}

impl<S: Clone> std::ops::Deref for ScalarSubqueryScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for ScalarSubqueryScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for ScalarSubqueryScope<'_, S> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.scalar_subqueries = previous;
            self.ctes.scalar_subquery_arena = self.previous_arena;
            self.ctes.lock_identities = self.previous_lock_identities;
        }
    }
}

pub struct VisibleCteScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: Option<BTreeSet<String>>,
}

impl<S: Clone> std::ops::Deref for VisibleCteScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for VisibleCteScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for VisibleCteScope<'_, S> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.visible_cte_names = previous;
        }
    }
}
