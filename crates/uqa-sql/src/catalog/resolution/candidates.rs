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
    let mut candidates = Vec::new();
    candidates.push(RelationIdentity::new(
        state.temporary_schema_name(),
        &relation,
    ));
    for schema in state.search_path().iter() {
        if schema == "pg_catalog" || schema == "information_schema" {
            continue;
        }
        candidates.push(RelationIdentity::new(schema, &relation));
    }
    Ok(candidates)
}
