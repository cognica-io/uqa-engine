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
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub trait StoredQuerySequences {
    fn query_sequence(&self, reference: &str) -> Result<String, String>;
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String>;

    /// External callbacks own name construction; their returned buffer transfers without copying. Native catalogs override this boundary to admit candidates and the selected name before allocation.
    fn query_sequence_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        sequences::external_name(|| self.query_sequence(reference), control)
    }

    /// Resolve only against already-loaded catalog state with the same ownership contract as the live lookup.
    fn loaded_query_sequence_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        sequences::external_name(|| self.loaded_query_sequence(reference), control)
    }
}
/// Namespace metadata captured at the start of a stored query binding pass.
pub struct StoredQueryNamespace {
    pub temporary_schema: String,
    pub transition_relations: BTreeSet<String>,
}
/// Both namespace representations provide the same exact captured transition-name membership.
pub trait TransitionRelationNames: Sync {
    fn is_empty(&self) -> bool;
    fn contains(&self, name: &str) -> bool;
}
impl TransitionRelationNames for BTreeSet<String> {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }
    fn contains(&self, name: &str) -> bool {
        self.contains(name)
    }
}
impl TransitionRelationNames for crate::binding::production::BindingNameSet {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }
    fn contains(&self, name: &str) -> bool {
        self.contains(name)
    }
}
pub struct StoredQueryBindingContext<
    'a,
    R: ?Sized = dyn StoredRelationCatalog + 'a,
    S: ?Sized = dyn StoredQuerySequences + 'a,
> {
    pub relations: &'a R,
    pub lookup_mode: RelationLookupMode,
    pub sequences: &'a S,
    pub temporary_schema: &'a str,
    pub transition_relations: &'a dyn TransitionRelationNames,
}

pub fn resolve_loaded_query_sequence(
    reference: &str,
    candidates: Vec<RelationIdentity>,
    contains: impl FnMut(&RelationIdentity) -> bool,
) -> Result<String, String> {
    sequences::resolve_loaded_query_sequence_with_control(
        reference,
        &candidates,
        contains,
        &ProductionControl::uncontrolled(),
    )
    .map(|name| name.into_uncontrolled().expect("ordinary sequence lookup"))
    .map_err(|error| match error {
        SQLError::Unsupported(message) => message,
        error => error.to_string(),
    })
}

pub fn bind_stored_query_relations<
    R: StoredRelationCatalog + ?Sized,
    S: StoredQuerySequences + ?Sized,
>(
    catalog: &StoredQueryBindingContext<'_, R, S>,
    plan: &mut QueryPlan,
    context: &str,
    reject_transition_relations: bool,
    loaded_catalog: bool,
) -> Result<bool, SQLError> {
    let mut uses_temporary_relation = false;
    bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
        let (canonical, temporary) = catalog.canonical_relation_with_control(
            reference,
            context,
            reject_transition_relations,
            loaded_catalog,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?;
        uses_temporary_relation |= temporary;
        Ok::<_, SQLError>(
            canonical
                .into_uncontrolled()
                .expect("ordinary stored binding"),
        )
    })?;
    bind_query_plan_sequence_references(plan, &mut |reference| {
        Ok::<_, SQLError>(
            catalog
                .canonical_sequence_with_control(
                    reference,
                    context,
                    loaded_catalog,
                    &ProductionControl::uncontrolled(),
                )?
                .into_uncontrolled()
                .expect("ordinary sequence binding"),
        )
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

mod production;
mod sequences;
pub use sequences::{resolve_query_sequence_with_control, select_query_sequence_with_control};

#[cfg(test)]
mod tests;
