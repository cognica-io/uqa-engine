//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Save and restore materialized, deferred, and non-returning CTE bindings.

use crate::query::CteScope;
use std::collections::BTreeSet;
use std::sync::Arc;

pub fn save_and_remove_cte_names<S: Clone>(
    ctes: &mut CteScope<S>,
    names: &BTreeSet<String>,
) -> Vec<SavedCteBinding> {
    let saved = save_cte_names(ctes, names);
    for name in names {
        ctes.remove_materialized(name);
        ctes.remove_deferred(name);
    }
    saved
}

/// Retain enclosing names while the new non-recursive definitions are evaluated; a declaration shadows its name only after its own inputs are established.
pub fn save_cte_names<S: Clone>(
    ctes: &CteScope<S>,
    names: &BTreeSet<String>,
) -> Vec<SavedCteBinding> {
    names
        .iter()
        .map(|name| SavedCteBinding {
            name: name.clone(),
            non_returning: ctes.non_returning_ctes.contains(name),
            rows: ctes.rows.get(name).cloned(),
            deferred: ctes.deferred_ctes.get(name).cloned(),
            definition: ctes.definition_scopes.get(name).cloned(),
            folded: ctes.folded_ctes.contains(name),
        })
        .collect()
}

pub fn restore_cte_names<S: Clone>(ctes: &mut CteScope<S>, saved: Vec<SavedCteBinding>) {
    for binding in saved {
        let name = binding.name.clone();
        ctes.remove_materialized(&binding.name);
        ctes.remove_deferred(&binding.name);
        if let Some(rows) = binding.rows {
            ctes.insert_rows(binding.name, rows);
        } else if let Some(plan) = binding.deferred {
            ctes.deferred_ctes.insert(binding.name, plan);
        }
        if binding.non_returning {
            ctes.non_returning_ctes.insert(name.clone());
        }
        if let Some(definition) = binding.definition {
            ctes.definition_scopes.insert(name.clone(), definition);
        }
        if binding.folded {
            ctes.folded_ctes.insert(name);
        }
    }
}

pub struct SavedCteBinding {
    name: String,
    non_returning: bool,
    rows: Option<crate::query::cte::rows::CteRows>,
    deferred: Option<uqa_sql::plan::RetainedCtePlan>,
    definition: Option<Arc<super::definitions::DefinitionBindings>>,
    folded: bool,
}
