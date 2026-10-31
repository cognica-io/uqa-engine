//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical inputs of deferred definitions, sharing plans and materialized readers.

use super::CteScope;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, ProductionControl};
use uqa_sql::{
    binding::production::{BindingMap, BindingNameSet},
    plan::{CtePlan, RetainedCtePlan},
    SQLError,
};

mod publication;
use publication::PendingEntry;

#[derive(Default)]
pub(super) struct DefinitionBindings {
    rows: super::CteRowsMap,
    deferred: BindingMap<RetainedCtePlan>,
    definitions: BindingMap<Arc<Self>>,
    non_returning: BindingNameSet,
    folded: BindingNameSet,
    recursive_widths: BindingMap<usize>,
    names: BindingNameSet,
    own_reference: bool,
    _memory: Option<MemoryReservation>,
}

impl DefinitionBindings {
    fn capture<S: Clone>(
        scope: &CteScope<S>,
        plan: &CtePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        let names = uqa_sql::semantics::cte_external_references_with_control(
            &plan.body,
            scope
                .rows
                .keys()
                .chain(scope.deferred_ctes.keys())
                .map(String::as_str),
            *control,
        )?;
        let own_reference =
            uqa_sql::semantics::cte_references_own_name_with_control(plan, *control)?;
        let mut retained = Self {
            own_reference,
            ..Self::default()
        };
        for name in names.iter().copied() {
            control.check()?;
            if own_reference && name == plan.name {
                continue;
            }
            if let Some(rows) = scope.rows.get(name) {
                retained
                    .rows
                    .insert_with_control(name, rows.clone(), control)?;
            }
            if let Some(plan) = scope.deferred_ctes.get(name) {
                retained
                    .deferred
                    .insert_with_control(name, plan.clone(), control)?;
            }
            if let Some(definition) = scope.definition_scopes.get(name) {
                retained
                    .definitions
                    .insert_with_control(name, Arc::clone(definition), control)?;
            }
            if scope.non_returning_ctes.contains(name) {
                retained.non_returning.insert_with_control(name, control)?;
            }
            if scope.folded_ctes.contains(name) {
                retained.folded.insert_with_control(name, control)?;
            }
            if let Some(width) = scope.recursive_control_widths.get(name) {
                retained
                    .recursive_widths
                    .insert_with_control(name, *width, control)?;
            }
            retained.names.insert_with_control(name, control)?;
        }
        let memory = control.reserve(size_of::<Self>())?;
        Ok(Arc::new(Self {
            _memory: memory,
            ..retained
        }))
    }

    fn scope<S: Clone>(
        &self,
        parent: &CteScope<S>,
        own_plan: Option<&RetainedCtePlan>,
        control: &ProductionControl<'_>,
    ) -> Result<CteScope<S>, SQLError> {
        let mut scope = parent.copy_context_with_control(control)?;
        scope.rows = self.rows.copy_with_control(control)?;
        scope.deferred_ctes = self
            .deferred
            .copy_with_control(control, |plan| Ok(plan.clone()))?;
        scope.definition_scopes = self
            .definitions
            .copy_with_control(control, |definition| Ok(Arc::clone(definition)))?;
        scope.non_returning_ctes = self.non_returning.copy_with_control(control)?;
        scope.folded_ctes = self.folded.copy_with_control(control)?;
        scope.recursive_control_widths = self
            .recursive_widths
            .copy_with_control(control, |width| Ok(*width))?;
        scope.visible_cte_names = self.names.copy_with_control(control)?;
        if self.own_reference {
            if let Some(plan) = own_plan {
                scope
                    .deferred_ctes
                    .insert_with_control(&plan.name, plan.clone(), control)?;
                scope
                    .visible_cte_names
                    .insert_with_control(&plan.name, control)?;
            }
        }
        Ok(scope)
    }
}

impl<S: Clone> CteScope<S> {
    /// Prepare a definition in its original lexical bindings while retaining the invoking statement's catalog, row locks, scalar arena and transaction controls.
    pub fn definition_scope(&self, reference: &str) -> Option<Self> {
        self.definition_scope_with_control(reference, &ProductionControl::uncontrolled())
            .expect("ordinary definition scope")
    }

    pub fn definition_scope_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        control.check()?;
        let Some(reference) = uqa_core::RelationIdentity::borrowed_reference(reference)
            .filter(|reference| reference.schema.is_none())
        else {
            return Ok(None);
        };
        self.definition_scopes
            .get_name(reference.name)
            .map(|retained| {
                retained.scope(self, self.deferred_ctes.get_name(reference.name), control)
            })
            .transpose()
    }

    /// A declaration already owns its decoded identifier; dots and quotes in that name are not SQL qualification syntax.
    #[cfg(test)]
    pub(crate) fn definition_scope_for_name(&self, name: &str) -> Option<Self> {
        self.definition_scope_for_name_with_control(name, &ProductionControl::uncontrolled())
            .expect("ordinary decoded definition scope")
    }

    pub(crate) fn definition_scope_for_name_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        control.check()?;
        self.definition_scopes
            .get(name)
            .map(|retained| retained.scope(self, self.deferred_ctes.get(name), control))
            .transpose()
    }

    pub(crate) fn insert_preparation_cte(&mut self, plan: RetainedCtePlan, folded: bool) {
        self.insert_preparation_cte_with_control(plan, folded, &ProductionControl::uncontrolled())
            .expect("ordinary CTE declaration");
    }

    /// Prepare every new name, node and selected definition before publishing any part of the declaration. Replacing a key reuses its exact original node.
    pub(crate) fn insert_preparation_cte_with_control(
        &mut self,
        plan: RetainedCtePlan,
        folded: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let definition = DefinitionBindings::capture(self, &plan, control)?;
        let definition =
            PendingEntry::prepare(&self.definition_scopes, &plan.name, definition, control)?;
        let deferred =
            PendingEntry::prepare(&self.deferred_ctes, &plan.name, plan.clone(), control)?;
        let mut non_returning = BindingNameSet::new();
        if !plan.body.returns_rows() && !self.non_returning_ctes.contains(&plan.name) {
            non_returning.insert_with_control(&plan.name, control)?;
        }
        let mut folded_names = BindingNameSet::new();
        if folded && !self.folded_ctes.contains(&plan.name) {
            folded_names.insert_with_control(&plan.name, control)?;
        }
        control.check()?;
        self.rows.remove(&plan.name);
        if plan.body.returns_rows() {
            self.non_returning_ctes.remove(&plan.name);
        }
        self.non_returning_ctes.append(non_returning);
        if !folded {
            self.folded_ctes.remove(&plan.name);
        }
        self.folded_ctes.append(folded_names);
        definition.publish(&mut self.definition_scopes, &plan.name);
        deferred.publish(&mut self.deferred_ctes, &plan.name);
        Ok(())
    }

    pub fn insert_deferred_with_control(
        &mut self,
        plan: RetainedCtePlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.insert_preparation_cte_with_control(plan, true, control)
    }

    pub(crate) fn cte_is_folded(&self, name: &str) -> bool {
        uqa_core::RelationIdentity::borrowed_reference(name).is_some_and(|reference| {
            reference.schema.is_none() && self.folded_ctes.contains_name(reference.name)
        })
    }

    #[cfg(test)]
    pub(crate) fn deferred_input(&self, name: &str) -> Option<(RetainedCtePlan, Self)> {
        self.deferred_input_with_control(name, &ProductionControl::uncontrolled())
            .expect("ordinary deferred input")
    }

    pub(crate) fn deferred_input_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<(RetainedCtePlan, Self)>, SQLError> {
        Ok(self
            .definition_scope_with_control(name, control)?
            .and_then(|scope| self.deferred_for_scan(name).map(|plan| (plan, scope))))
    }
}
