//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Save and restore materialized, deferred, and non-returning CTE bindings.

use crate::query::CteScope;
use std::collections::BTreeSet;
use uqa_sql::plan::CtePlan;

pub fn save_and_remove_cte_names<S: Clone>(
    ctes: &mut CteScope<S>,
    names: &BTreeSet<String>,
) -> Vec<SavedCteBinding> {
    names
        .iter()
        .map(|name| SavedCteBinding {
            name: name.clone(),
            non_returning: ctes.non_returning_ctes.contains(name),
            rows: ctes.remove_materialized(name),
            deferred: ctes.remove_deferred(name),
        })
        .collect()
}

pub fn restore_cte_names<S: Clone>(ctes: &mut CteScope<S>, saved: Vec<SavedCteBinding>) {
    for binding in saved {
        let name = binding.name.clone();
        ctes.remove_materialized(&binding.name);
        ctes.remove_deferred(&binding.name);
        if let Some(rows) = binding.rows {
            ctes.insert_shared(binding.name, rows);
        } else if let Some(plan) = binding.deferred {
            ctes.insert_deferred(plan);
        }
        if binding.non_returning {
            ctes.non_returning_ctes.insert(name);
        }
    }
}

pub struct SavedCteBinding {
    name: String,
    non_returning: bool,
    rows: Option<crate::SharedSpill>,
    deferred: Option<CtePlan>,
}
