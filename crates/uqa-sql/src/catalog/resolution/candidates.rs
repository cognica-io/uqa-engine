//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy relation candidates with lazy session input reads and exact search-path ordering.
#[cfg(test)]
mod tests;
use crate::SQLError;
use std::ops::Deref;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_core::RelationIdentity;
mod production;
pub use production::relation_lookup_candidates_with_control;
pub type SearchPathRead<'a> = Box<dyn Deref<Target = Vec<String>> + 'a>;
/// A search path borrowed under its session read guard, with session placeholders already resolved.
pub trait SearchPathView {
    fn len(&self) -> usize;
    fn schema(&self, index: usize) -> &str;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl SearchPathView for Vec<String> {
    fn len(&self) -> usize {
        self.len()
    }
    fn schema(&self, index: usize) -> &str {
        &self[index]
    }
}
pub trait RelationCandidateState {
    fn temporary_schema_name(&self) -> String;
    fn search_path(&self) -> SearchPathRead<'_>;
    fn temporary_schema_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        control.check()?;
        Ok(control.copy_text(&self.temporary_schema_name())?)
    }
    fn with_search_path_with_control(
        &self,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&dyn SearchPathView) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let path = self.search_path();
        read(&**path)
    }
}
pub fn relation_lookup_candidates(
    state: &dyn RelationCandidateState,
    name: &str,
) -> Result<Vec<RelationIdentity>, String> {
    relation_lookup_candidates_with_control(state, name, &ProductionControl::uncontrolled())
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary relation candidates")
        })
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            other => other.to_string(),
        })
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
