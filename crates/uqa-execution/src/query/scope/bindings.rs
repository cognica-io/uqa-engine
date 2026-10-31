//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Save selected lexical bindings and restore their exact nodes without allocating.

use super::{definitions::DefinitionBindings, CteRowsMap, CteScope};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    binding::production::{BindingMap, BindingNameSet},
    plan::RetainedCtePlan,
    SQLError,
};

#[derive(Default)]
pub struct SavedCteBindings {
    names: BindingNameSet,
    rows: CteRowsMap,
    deferred: BindingMap<RetainedCtePlan>,
    definitions: BindingMap<Arc<DefinitionBindings>>,
    non_returning: BindingNameSet,
    folded: BindingNameSet,
}

pub fn save_and_remove_cte_names<S: Clone>(
    ctes: &mut CteScope<S>,
    names: &BTreeSet<String>,
) -> SavedCteBindings {
    save_and_remove_cte_names_with_control(
        ctes,
        names.iter().map(String::as_str),
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary lexical extraction")
}

pub fn save_and_remove_cte_names_with_control<'n, S: Clone>(
    ctes: &mut CteScope<S>,
    names: impl IntoIterator<Item = &'n str>,
    control: &ProductionControl<'_>,
) -> Result<SavedCteBindings, SQLError> {
    let mut saved = SavedCteBindings::default();
    for name in names {
        saved.names.insert_with_control(name, control)?;
    }
    control.check()?;
    for name in &saved.names {
        saved.rows.append(ctes.rows.extract(name));
        saved.deferred.append(ctes.deferred_ctes.extract(name));
        saved
            .definitions
            .append(ctes.definition_scopes.extract(name));
        saved
            .non_returning
            .append(ctes.non_returning_ctes.extract(name));
        saved.folded.append(ctes.folded_ctes.extract(name));
    }
    Ok(saved)
}

/// Retain enclosing names while non-recursive definitions establish their inputs. No part of the live scope changes during admission.
pub fn save_cte_names<S: Clone>(ctes: &CteScope<S>, names: &BTreeSet<String>) -> SavedCteBindings {
    save_cte_names_with_control(
        ctes,
        names.iter().map(String::as_str),
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary lexical copy")
}

pub fn save_cte_names_with_control<'n, S: Clone>(
    ctes: &CteScope<S>,
    names: impl IntoIterator<Item = &'n str>,
    control: &ProductionControl<'_>,
) -> Result<SavedCteBindings, SQLError> {
    control.check()?;
    let mut saved = SavedCteBindings::default();
    for name in names {
        if !saved.names.insert_with_control(name, control)? {
            continue;
        }
        if let Some(rows) = ctes.rows.get(name) {
            saved
                .rows
                .insert_with_control(name, rows.clone(), control)?;
        }
        if let Some(plan) = ctes.deferred_ctes.get(name) {
            saved
                .deferred
                .insert_with_control(name, plan.clone(), control)?;
        }
        if let Some(definition) = ctes.definition_scopes.get(name) {
            saved
                .definitions
                .insert_with_control(name, Arc::clone(definition), control)?;
        }
        if ctes.non_returning_ctes.contains(name) {
            saved.non_returning.insert_with_control(name, control)?;
        }
        if ctes.folded_ctes.contains(name) {
            saved.folded.insert_with_control(name, control)?;
        }
    }
    Ok(saved)
}

pub fn restore_cte_names<S: Clone>(ctes: &mut CteScope<S>, saved: SavedCteBindings) {
    for name in &saved.names {
        ctes.remove_materialized(name);
        ctes.remove_deferred(name);
    }
    ctes.rows.append(saved.rows);
    ctes.deferred_ctes.append(saved.deferred);
    ctes.definition_scopes.append(saved.definitions);
    ctes.non_returning_ctes.append(saved.non_returning);
    ctes.folded_ctes.append(saved.folded);
}

pub struct CteBindingScope<'a, S: Clone> {
    scope: &'a mut CteScope<S>,
    saved: Option<SavedCteBindings>,
}

impl<S: Clone> CteScope<S> {
    /// Restore only the selected names on every exit, preserving progress of unrelated materialized inputs.
    pub fn enter_cte_bindings_with_control<'a, 'n>(
        &'a mut self,
        names: impl IntoIterator<Item = &'n str>,
        control: &ProductionControl<'_>,
    ) -> Result<CteBindingScope<'a, S>, SQLError> {
        let saved = save_cte_names_with_control(self, names, control)?;
        Ok(CteBindingScope {
            scope: self,
            saved: Some(saved),
        })
    }
}
impl<S: Clone> std::ops::Deref for CteBindingScope<'_, S> {
    type Target = CteScope<S>;
    fn deref(&self) -> &Self::Target {
        self.scope
    }
}
impl<S: Clone> std::ops::DerefMut for CteBindingScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.scope
    }
}
impl<S: Clone> Drop for CteBindingScope<'_, S> {
    fn drop(&mut self) {
        restore_cte_names(self.scope, self.saved.take().expect("live lexical guard"));
    }
}
