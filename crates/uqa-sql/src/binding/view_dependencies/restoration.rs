//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore legacy view source names and scalar dispatches from explicit catalog identities.
use super::{bind_query_plan_relations, canonical_virtual_relation_reference};
use crate::{ast::FunctionBinding, plan::QueryPlan};
use uqa_core::RelationIdentity;

pub fn upgrade_legacy_view_dispatches(plan: &mut QueryPlan) -> bool {
    let mut changed = false;
    plan.rewrite_scalar_expressions(&mut |expression| {
        let crate::ScalarExpr::Func { name, binding, .. } = expression else {
            return;
        };
        changed |= FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
    });
    changed
}

pub fn bind_stored_view_relations(
    plan: &mut QueryPlan,
    relations: &std::collections::BTreeSet<RelationIdentity>,
) -> Result<(), String> {
    bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
        if let Some(canonical) = canonical_virtual_relation_reference(reference) {
            return Ok(canonical);
        }
        let (schema, local_name) = RelationIdentity::parse_reference(reference)
            .map_err(|error| format!("invalid stored view source `{reference}`: {error}"))?;
        if let Some(schema) = schema {
            let candidate = RelationIdentity::new(schema, local_name);
            if relations.contains(&candidate) {
                return Ok(candidate.qualified_name());
            }
        } else {
            let candidates = relations
                .iter()
                .filter(|candidate| candidate.name == local_name)
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [candidate] => return Ok(candidate.clone()),
                [] => {}
                _ => {
                    return Err(format!(
                        "ambiguous stored view source `{reference}` matches {}",
                        candidates.join(", ")
                    ));
                }
            }
        }
        Err(format!(
            "stored view source relation `{reference}` does not exist"
        ))
    })
}

#[cfg(test)]
mod tests;
