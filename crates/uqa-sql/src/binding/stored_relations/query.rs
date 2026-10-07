//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query relation and sequence binding against current or already-loaded catalog state.

use super::StoredRelationCatalog;
use crate::{
    binding::view_dependencies::{bind_query_plan_relations, bind_query_plan_sequence_references},
    catalog::resolution::RelationLookupMode,
    plan::QueryPlan,
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub trait StoredQuerySequences {
    fn query_sequence(&self, reference: &str) -> Result<String, String>;
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String>;
}
/// Namespace metadata captured at the start of a stored query binding pass.
pub struct StoredQueryNamespace {
    pub temporary_schema: String,
    pub transition_relations: BTreeSet<String>,
}
pub struct StoredQueryBindingContext<'a> {
    pub relations: &'a dyn StoredRelationCatalog,
    pub lookup_mode: RelationLookupMode,
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
        if catalog.lookup_mode == RelationLookupMode::Dynamic {
            if let Some(canonical) = catalog
                .relations
                .resolve_age_label_relation_name(reference)?
            {
                return Ok(canonical);
            }
        }
        if RelationIdentity::parse_reference(reference).is_ok_and(|(schema, relation)| {
            schema.is_none() && catalog.transition_relations.contains(&relation)
        }) {
            if reject_transition_relations {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "transition tables cannot be referenced in a view definition".into(),
                });
            }
            return Ok(reference.to_string());
        }
        let resolved = match (catalog.lookup_mode, loaded_catalog) {
            (RelationLookupMode::Bound, _) => {
                catalog.relations.resolve_bound_relation_kind(reference)?
            }
            (RelationLookupMode::Dynamic, true) => catalog
                .relations
                .resolve_loaded_visible_relation_kind(reference)?,
            (RelationLookupMode::Dynamic, false) => {
                catalog.relations.resolve_visible_relation_kind(reference)?
            }
        }
        .into_found();
        match resolved {
            Some((canonical, "table" | "view" | "materialized view" | "foreign table")) => {
                uses_temporary_relation |= RelationIdentity::from_legacy_name(&canonical)
                    .is_ok_and(|relation| relation.schema == catalog.temporary_schema);
                Ok(canonical)
            }
            Some((canonical, "composite type")) => Err(composite_relation_error(&canonical)),
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

/// A composite type's relation holds no rows, so `table_open` refuses it.
pub(super) fn composite_relation_error(canonical: &str) -> SQLError {
    crate::catalog::analysis::UnopenableRelation {
        name: RelationIdentity::from_legacy_name(canonical)
            .map_or_else(|_| canonical.to_string(), |relation| relation.name),
        kinds: "composite types",
    }
    .error()
}

#[cfg(test)]
mod tests;
