//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical target and stored-rule metadata for SQL validation.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_sql::{ast::RuleEvent, catalog::events::StoredRule, SQLError};

impl uqa_sql::semantics::rules::RuleCatalog for Engine {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        Engine::relation_has_rules(self, table)
    }
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError> {
        Engine::resolve_rule_relation(self, table)
    }
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Engine::rules_for(self, table, event)
    }
    fn resolve_mutation_target(&self, name: &str, bound: bool) -> Result<String, SQLError> {
        self.resolve_mutation_target_name(name, bound)
    }
}

impl uqa_sql::semantics::rules::action_binding::RuleSourceCatalog for Engine {
    fn query_source_columns(
        &self,
        name: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError> {
        uqa_execution::catalog::projection::query_source_column_names(
            &self.catalog_execution(),
            name,
            relations_bound,
        )
    }
    fn rule_relation_columns(
        &self,
        name: &str,
    ) -> Result<Vec<(String, uqa_sql::ColumnType)>, SQLError> {
        Engine::rule_relation_columns(self, name)
    }
}
