//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite bound relation identities while preserving each stored view's public metadata.
use super::StoredView;
use crate::binding::view_dependencies::{
    bind_query_plan_relations, query_plan_references_relation,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub fn rewritten_relation_references(
    views: &impl crate::catalog::named_definitions::NamedDefinitionView<StoredView, RelationIdentity>,
    replacements: &BTreeMap<RelationIdentity, RelationIdentity>,
) -> Result<Vec<(RelationIdentity, StoredView)>, String> {
    let mut updates = Vec::new();
    let empty_ctes = std::collections::BTreeSet::new();
    for (view_relation, stored) in views.iter() {
        if !replacements
            .keys()
            .any(|relation| query_plan_references_relation(&stored.query, relation, &empty_ctes))
        {
            continue;
        }
        let mut candidate = stored.clone();
        let mut changed = false;
        bind_query_plan_relations(
            &mut candidate.query,
            &std::collections::BTreeSet::new(),
            &mut |reference| -> Result<String, String> {
                let identity = RelationIdentity::from_legacy_name(reference)?;
                if let Some(replacement) = replacements.get(&identity) {
                    changed = true;
                    Ok(replacement.qualified_name())
                } else {
                    Ok(reference.to_string())
                }
            },
        )?;
        if changed {
            updates.push((view_relation.clone(), candidate));
        }
    }
    Ok(updates)
}
