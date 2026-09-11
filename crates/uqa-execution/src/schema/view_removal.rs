//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View deletion preflight and dependency scheduling.
pub mod context;
mod publication;
use super::view_dependencies;
use context::{ViewRemovalContext, ViewRemovalTransactions};

use uqa_core::RelationIdentity;
use uqa_sql::{catalog::stored_view::removal as analysis, SQLError};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub fn drop_view(
    transactions: &impl ViewRemovalTransactions,
    name: &str,
) -> Result<bool, SQLError> {
    transactions.with_view_removal(|transactions, context| {
        let target = analysis::direct_view_drop_target(context.names.relation_kind(name)?)?;
        if let Some(canonical) = target {
            drop_views(transactions, &[canonical], false, "view")?;
            Ok(true)
        } else {
            Ok(false)
        }
    })
}
pub fn drop_views(
    transactions: &impl ViewRemovalTransactions,
    names: &[String],
    cascade: bool,
    kind: &str,
) -> Result<(), SQLError> {
    transactions.with_view_removal(|_, context| {
        ensure_view_drop_authorities(context, names)?;
        context
            .routines
            .drop_relation_routine_dependents(names, cascade, kind)?;
        if !cascade {
            return drop_views_inner(context, names, false);
        }
        let remaining = remaining_view_drop_targets(context, names)?;
        let closure = view_dependencies::cascade_view_closure(&context.dependencies, remaining)?;
        context
            .events
            .drop_rules_depending_on_relations_inner(&closure)
            .map_err(|error| {
                SQLError::Internal(format!("drop rules depending on cascading views: {error}"))
            })?;
        drop_views_inner(context, &closure, false)
    })
}
fn ensure_view_drop_authorities(
    context: &ViewRemovalContext<'_>,
    names: &[String],
) -> Result<(), SQLError> {
    let views = context.registry.views_read();
    analysis::ensure_view_drop_authorities(context.ownership, names, &views)
}

pub fn remaining_view_drop_targets(
    context: &ViewRemovalContext<'_>,
    names: &[String],
) -> Result<Vec<String>, SQLError> {
    let remaining = names
        .iter()
        .filter_map(|name| match RelationIdentity::from_legacy_name(name) {
            Ok(identity) => context
                .registry
                .views_read()
                .contains_key(&identity)
                .then(|| Ok(name.clone())),
            Err(error) => Some(Err(SQLError::Internal(error))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(remaining)
}

pub fn drop_views_depending_on_relations(
    context: &ViewRemovalContext<'_>,
    relations: &[String],
) -> StorageBackendResult<()> {
    context
        .routines
        .drop_relation_routine_dependents(relations, true, "relation")
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    let mut pending = relations.to_vec();
    let mut views = std::collections::BTreeSet::new();
    while let Some(relation) = pending.pop() {
        for dependent in
            view_dependencies::views_depending_on_relation(&context.dependencies, &relation)?
        {
            if views.insert(dependent.clone()) {
                pending.push(dependent);
            }
        }
    }
    let views = views.into_iter().collect::<Vec<_>>();
    context
        .events
        .drop_rules_depending_on_relations_inner(&views)?;
    drop_views_inner(context, &views, false)
        .map_err(|error| StorageBackendError::Other(error.to_string()))
}

pub fn drop_views_inner(
    context: &ViewRemovalContext<'_>,
    names: &[String],
    check_authority: bool,
) -> Result<(), SQLError> {
    let drop_set = names
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if check_authority {
        ensure_view_drop_authorities(context, names)?;
    }
    let dependent_rules = context
        .events
        .rules_depending_on_relations(names)
        .map_err(|error| SQLError::Internal(format!("inspect rule dependencies: {error}")))?;
    analysis::ensure_no_rule_dependents(names, dependent_rules)?;
    for name in names {
        let dependents =
            view_dependencies::views_depending_on_relation(&context.dependencies, name)
                .map_err(|err| SQLError::Internal(format!("inspect view dependencies: {err}")))?
                .into_iter()
                .filter(|dependent| !drop_set.contains(dependent))
                .collect::<Vec<_>>();
        analysis::ensure_no_view_dependents(name, &dependents)?;
    }
    for name in names {
        drop_view_state_inner(context, name)?;
    }
    Ok(())
}

pub fn drop_temporary_views_depending_on_relation_inner(
    context: &ViewRemovalContext<'_>,
    canonical_name: &str,
) -> StorageBackendResult<()> {
    let target =
        RelationIdentity::from_legacy_name(canonical_name).map_err(StorageBackendError::Other)?;
    let views = context.registry.views_read();
    let layers = analysis::temporary_view_dependency_layers(canonical_name, target, &views)
        .map_err(StorageBackendError::Other)?;
    drop(views);
    // Internal ON COMMIT deletion removes the outermost dependent views first so no temporary view retains a dangling relation binding.
    for layer in layers.into_iter().rev() {
        for relation in layer {
            let name = relation.qualified_name();
            drop_view_state_inner(context, &name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
    }
    Ok(())
}

fn drop_view_state_inner(context: &ViewRemovalContext<'_>, name: &str) -> Result<(), SQLError> {
    publication::drop_view_state_inner(
        context.registry,
        context.publication,
        context.events,
        context.changes,
        name,
    )
}
