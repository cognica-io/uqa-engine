//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy relation candidates with lazy session input reads and exact search-path ordering.
#[cfg(test)]
mod tests;
use std::ops::Deref;
use uqa_core::RelationIdentity;
pub type SearchPathRead<'a> = Box<dyn Deref<Target = Vec<String>> + 'a>;
pub trait RelationCandidateState {
    fn temporary_schema_name(&self) -> String;
    fn search_path(&self) -> SearchPathRead<'_>;
}
pub fn relation_lookup_candidates(
    state: &dyn RelationCandidateState,
    name: &str,
) -> Result<Vec<RelationIdentity>, String> {
    let (schema, relation) = RelationIdentity::parse_reference(name)?;
    if let Some(schema) = schema {
        if schema == "pg_temp" {
            return Ok(vec![RelationIdentity::new(
                state.temporary_schema_name(),
                relation,
            )]);
        }
        return Ok(vec![RelationIdentity::new(schema, relation)]);
    }
    let temporary = state.temporary_schema_name();
    let path = state.search_path();
    Ok(unqualified_candidates(&temporary, &path, &relation))
}

pub(super) fn unqualified_candidates(
    temporary: &str,
    path: &[String],
    relation: &str,
) -> Vec<RelationIdentity> {
    let mut candidates = Vec::new();
    if !path
        .iter()
        .any(|schema| schema == "pg_temp" || schema == temporary)
    {
        candidates.push(RelationIdentity::new(temporary, relation));
    }
    if !path.iter().any(|schema| schema == "pg_catalog") {
        candidates.push(RelationIdentity::new("pg_catalog", relation));
    }
    candidates.extend(path.iter().map(|schema| {
        RelationIdentity::new(
            if schema == "pg_temp" {
                temporary
            } else {
                schema
            },
            relation,
        )
    }));
    candidates
}
