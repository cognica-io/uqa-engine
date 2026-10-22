//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical inputs of deferred definitions, sharing plans and materialized readers.

use super::CteScope;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uqa_sql::plan::CtePlan;

#[derive(Default)]
pub(super) struct DefinitionBindings {
    rows: BTreeMap<String, crate::SharedSpill>,
    deferred: BTreeMap<String, Arc<CtePlan>>,
    definitions: BTreeMap<String, Arc<Self>>,
    non_returning: BTreeSet<String>,
    folded: BTreeSet<String>,
    recursive_widths: BTreeMap<String, usize>,
    names: BTreeSet<String>,
}

impl DefinitionBindings {
    pub(super) fn capture<S: Clone>(scope: &CteScope<S>, plan: &CtePlan) -> Self {
        let names = scope
            .rows
            .keys()
            .chain(scope.deferred_ctes.keys())
            .cloned()
            .collect();
        let mut names = uqa_sql::semantics::cte_external_references(&plan.body, &names);
        if uqa_sql::semantics::cte_references_own_name(plan) {
            names.remove(&plan.name);
        }
        let mut retained = Self::default();
        for name in &names {
            if let Some(rows) = scope.rows.get(name) {
                retained.rows.insert(name.clone(), rows.clone());
            }
            if let Some(plan) = scope.deferred_ctes.get(name) {
                retained.deferred.insert(name.clone(), Arc::clone(plan));
            }
            if let Some(definition) = scope.definition_scopes.get(name) {
                retained
                    .definitions
                    .insert(name.clone(), Arc::clone(definition));
            }
            if scope.non_returning_ctes.contains(name) {
                retained.non_returning.insert(name.clone());
            }
            if scope.folded_ctes.contains(name) {
                retained.folded.insert(name.clone());
            }
            if let Some(width) = scope.recursive_control_widths.get(name) {
                retained.recursive_widths.insert(name.clone(), *width);
            }
        }
        retained.names = names;
        retained
    }
}

impl<S: Clone> CteScope<S> {
    /// Prepare a definition in its original lexical bindings while retaining the invoking statement's catalog, row locks, scalar arena and transaction controls.
    pub fn definition_scope(&self, reference: &str) -> Option<Self> {
        let name = uqa_sql::semantics::cte_reference_name(reference)?;
        self.definition_scope_for_name(&name)
    }

    /// A declaration already owns its decoded identifier; dots and quotes in that name are not SQL qualification syntax.
    pub(crate) fn definition_scope_for_name(&self, name: &str) -> Option<Self> {
        let retained = self.definition_scopes.get(name)?;
        let mut scope = self.clone();
        scope.rows.clone_from(&retained.rows);
        scope.deferred_ctes.clone_from(&retained.deferred);
        scope.definition_scopes.clone_from(&retained.definitions);
        scope.non_returning_ctes.clone_from(&retained.non_returning);
        scope.folded_ctes.clone_from(&retained.folded);
        scope
            .recursive_control_widths
            .clone_from(&retained.recursive_widths);
        scope.visible_cte_names.clone_from(&retained.names);
        if let Some(plan) = self.deferred_ctes.get(name) {
            if uqa_sql::semantics::cte_references_own_name(plan) {
                scope
                    .deferred_ctes
                    .insert(name.to_owned(), Arc::clone(plan));
                scope.visible_cte_names.insert(name.to_owned());
            }
        }
        Some(scope)
    }

    pub(crate) fn insert_preparation_cte(&mut self, plan: CtePlan, folded: bool) {
        let definition = Arc::new(DefinitionBindings::capture(self, &plan));
        self.rows.remove(&plan.name);
        if plan.body.returns_rows() {
            self.non_returning_ctes.remove(&plan.name);
        } else {
            self.non_returning_ctes.insert(plan.name.clone());
        }
        if folded {
            self.folded_ctes.insert(plan.name.clone());
        } else {
            self.folded_ctes.remove(&plan.name);
        }
        self.definition_scopes.insert(plan.name.clone(), definition);
        self.deferred_ctes.insert(plan.name.clone(), Arc::new(plan));
    }

    pub(crate) fn cte_is_folded(&self, name: &str) -> bool {
        uqa_sql::semantics::cte_reference_name(name)
            .is_some_and(|name| self.folded_ctes.contains(&name))
    }

    pub(crate) fn take_deferred_input(&mut self, name: &str) -> Option<(CtePlan, Self)> {
        let scope = self.definition_scope(name)?;
        self.deferred_for_scan(name).map(|plan| (plan, scope))
    }
}
