//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query relation and sequence binding against current or already-loaded catalog state.

use super::StoredRelationCatalog;
use crate::{
    binding::view_dependencies::{
        bind_query_plan_relations, bind_query_plan_sequence_references,
        canonical_virtual_relation_reference,
    },
    plan::QueryPlan,
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub trait StoredQuerySequences {
    fn query_sequence(&self, reference: &str) -> Result<String, String>;
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String>;
}
pub struct StoredQueryBindingContext<'a> {
    pub relations: &'a dyn StoredRelationCatalog,
    pub sequences: &'a dyn StoredQuerySequences,
    pub temporary_schema: &'a str,
    pub transition_relations: &'a BTreeSet<String>,
}

pub fn resolve_loaded_query_sequence(
    reference: &str,
    candidates: Vec<RelationIdentity>,
    mut contains: impl FnMut(&RelationIdentity) -> bool,
) -> Result<String, String> {
    candidates
        .into_iter()
        .find(|candidate| contains(candidate))
        .map(|candidate| candidate.qualified_name())
        .ok_or_else(|| format!("Sequence `{reference}` does not exist"))
}

pub fn bind_stored_query_relations(
    catalog: &StoredQueryBindingContext<'_>,
    plan: &mut QueryPlan,
    context: &str,
    reject_transition_relations: bool,
    loaded_catalog: bool,
) -> Result<bool, SQLError> {
    let mut uses_temporary_relation = false;
    bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
        // Catalog relations win for their supported spellings just as they do in FROM execution (notably unqualified `pg_class`). Explicit user schemas remain ordinary catalog identities.
        if let Some(canonical) = canonical_virtual_relation_reference(reference) {
            return Ok(canonical);
        }
        if let Some(canonical) = catalog
            .relations
            .resolve_age_label_relation_name(reference)?
        {
            return Ok(canonical);
        }
        if RelationIdentity::parse_reference(reference)
            .ok()
            .is_some_and(|(schema, relation)| {
                schema.is_none() && catalog.transition_relations.contains(&relation)
            })
        {
            if reject_transition_relations {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "transition tables cannot be referenced in a view definition".into(),
                });
            }
            return Ok(reference.to_string());
        }
        let resolved = if loaded_catalog {
            catalog
                .relations
                .resolve_loaded_visible_relation_kind(reference)?
                .into_found()
        } else {
            catalog
                .relations
                .resolve_visible_relation_kind(reference)?
                .into_found()
        };
        match resolved {
            Some((canonical, "table" | "view" | "materialized view" | "foreign table")) => {
                uses_temporary_relation |= RelationIdentity::from_legacy_name(&canonical)
                    .is_ok_and(|relation| relation.schema == catalog.temporary_schema);
                Ok(canonical)
            }
            Some((canonical, kind)) => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "{context} source \"{canonical}\" is a {kind}, not a row relation"
                ),
            }),
            None => Err(SQLError::UnknownTable(reference.to_string())),
        }
    })?;
    bind_query_plan_sequence_references(plan, &mut |reference| {
        let resolved = if loaded_catalog {
            catalog.sequences.loaded_query_sequence(reference)
        } else {
            catalog.sequences.query_sequence(reference)
        };
        resolved.map_err(|error| {
            SQLError::Unsupported(format!(
                "{context} sequence reference `{reference}`: {error}"
            ))
        })
    })?;
    Ok(uses_temporary_relation)
}
