//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored relation analysis to catalog lookup and retained sequence registry state.

use crate::Engine;
use uqa_sql::{
    ast::{Expr, Statement},
    binding::stored_relations::{
        self as analysis, StoredQueryBindingContext, StoredQuerySequences, StoredRelationCatalog,
    },
    catalog::{
        events::RuleDependencies,
        resolution::{RelationLookupMode, RelationResolution},
    },
    plan::QueryPlan,
    SQLError,
};

impl StoredRelationCatalog for Engine {
    fn resolve_age_label_relation_name(&self, reference: &str) -> Result<Option<String>, SQLError> {
        uqa_execution::catalog::projection::resolve_age_label_relation_name(
            &self.catalog_execution(),
            reference,
        )
    }
    fn resolve_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        Engine::resolve_visible_relation_kind(self, reference)
    }
    fn resolve_loaded_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        Engine::resolve_loaded_visible_relation_kind(self, reference)
    }
    fn resolve_bound_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        Engine::resolve_bound_relation_kind(self, reference)
    }
}
impl StoredQuerySequences for Engine {
    fn query_sequence(&self, reference: &str) -> Result<String, String> {
        self.resolve_sequence_reference_for_binding(reference)
            .map_err(|error| error.to_string())
    }
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String> {
        let sequences = self.durable.sequences.read();
        let candidates = self
            .relation_lookup_candidates(reference)
            .map_err(|error| error.to_string())?;
        analysis::resolve_loaded_query_sequence(reference, candidates, |candidate| {
            sequences.contains_key(candidate)
        })
    }
}
impl Engine {
    pub(crate) fn bind_stored_query_relations(
        &self,
        plan: &mut QueryPlan,
        context: &str,
        reject_transition_relations: bool,
    ) -> Result<bool, SQLError> {
        let temporary_schema = self.temporary_schema_name();
        let transition_relations = crate::sql::active_trigger_transition_relation_names();
        analysis::bind_stored_query_relations(
            &StoredQueryBindingContext {
                relations: self,
                sequences: self,
                temporary_schema: &temporary_schema,
                transition_relations: &transition_relations,
            },
            plan,
            context,
            reject_transition_relations,
            false,
        )
    }
    pub(crate) fn bind_rule_action_relation_dependencies(
        &self,
        statement: &mut Statement,
        lookup_mode: RelationLookupMode,
    ) -> Result<RuleDependencies, SQLError> {
        analysis::bind_rule_action_relation_dependencies(self, statement, lookup_mode)
    }
    pub(crate) fn bind_rule_condition_relation_dependencies(
        &self,
        expression: &mut Expr,
        lookup_mode: RelationLookupMode,
    ) -> Result<RuleDependencies, SQLError> {
        analysis::bind_rule_condition_relation_dependencies(self, expression, lookup_mode)
    }
}
