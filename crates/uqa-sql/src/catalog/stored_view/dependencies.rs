//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select stored view dependencies and rewrite exact routine identities in their bound plans.

use super::StoredView;
use crate::{
    ast::FunctionBinding,
    binding::view_dependencies::{
        query_plan_references_function, query_plan_references_relation,
        query_plan_references_sequence, rewrite_query_plan_routine_identity,
    },
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

/// Canonical bound sources make dependency checks exact; malformed in-memory plans remain conservative to prevent dangling DDL.
pub fn views_depending_on_relation(
    views: &BTreeMap<RelationIdentity, StoredView>,
    target: &RelationIdentity,
) -> Vec<String> {
    let empty_ctes = BTreeSet::new();
    let mut dependents = views
        .iter()
        .filter(|(relation, view)| {
            *relation != target && query_plan_references_relation(&view.query, target, &empty_ctes)
        })
        .map(|(relation, _)| relation.qualified_name())
        .collect::<Vec<_>>();
    dependents.sort_unstable();
    dependents
}

pub fn views_depending_on_sequence(
    views: &BTreeMap<RelationIdentity, StoredView>,
    target: &RelationIdentity,
) -> Vec<String> {
    let mut dependents = views
        .iter()
        .filter(|(_, view)| query_plan_references_sequence(&view.query, target))
        .map(|(relation, _)| relation.qualified_name())
        .collect::<Vec<_>>();
    dependents.sort_unstable();
    dependents
}

/// Return type is excluded from the exact non-builtin routine identity.
pub fn views_depending_on_function(
    views: &BTreeMap<RelationIdentity, StoredView>,
    target: &FunctionBinding,
) -> Vec<String> {
    let mut dependents = views
        .iter()
        .filter(|(_, view)| query_plan_references_function(&view.query, target))
        .map(|(relation, _)| relation.qualified_name())
        .collect::<Vec<_>>();
    dependents.sort_unstable();
    dependents
}

pub fn rewrite_view_routine_identity(
    views: &mut BTreeMap<RelationIdentity, StoredView>,
    target: &FunctionBinding,
    new_name: &str,
) -> Vec<RelationIdentity> {
    let mut changed = Vec::new();
    for (relation, view) in views {
        if rewrite_query_plan_routine_identity(&mut view.query, target, new_name) {
            changed.push(relation.clone());
        }
    }
    changed
}
