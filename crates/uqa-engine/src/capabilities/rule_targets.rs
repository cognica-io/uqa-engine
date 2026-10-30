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
        self.event_lookup_context().relation_has_rules(table)
    }
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError> {
        self.event_analysis_context().resolve_rule_relation(table)
    }
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        self.event_lookup_context().rules_for(table, event)
    }
    fn capture_rules(
        &self,
        table: &str,
        event: RuleEvent,
    ) -> Result<uqa_sql::catalog::events::selection::CapturedRules, SQLError> {
        let retained = self.query_retention_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.query_runtime_view().cancellation,
        );
        self.capture_rules_with_control(table, event, &control)
    }
    fn capture_rules_with_control(
        &self,
        table: &str,
        event: RuleEvent,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::catalog::events::selection::CapturedRules, SQLError> {
        let relation = self
            .event_analysis_context()
            .resolve_rule_relation_with_control(table, control)?;
        let replica = self.session_replication_role_is_replica_with_control(control)?;
        uqa_sql::catalog::events::selection::CapturedRules::from_catalog(
            self.durable.rules.snapshot(),
            relation,
            event,
            replica,
            control,
        )
    }
    fn resolve_mutation_target(&self, name: &str, bound: bool) -> Result<String, SQLError> {
        self.resolve_mutation_target_name(name, bound)
    }
    fn resolve_rule_relation_with_control(
        &self,
        table: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RelationIdentity>, SQLError> {
        self.event_analysis_context()
            .resolve_rule_relation_with_control(table, control)
    }
    fn resolve_mutation_target_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        self.resolve_mutation_target_name_with_control(name, bound, control)
    }
    fn visit_rules_with_control(
        &self,
        table: &str,
        event: RuleEvent,
        reverse: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
        visit: &mut dyn FnMut(&StoredRule) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let relation = self
            .event_analysis_context()
            .resolve_rule_relation_with_control(table, control)?;
        let replica = self.session_replication_role_is_replica_with_control(control)?;
        let rules = self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.rules.snapshot(),
            |snapshot| snapshot.rules.clone(),
        );
        uqa_sql::catalog::events::selection::visit_active_rules_with_control(
            &rules, &relation, event, replica, reverse, control, visit,
        )
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
    fn rule_relation_definitions(
        &self,
        name: &str,
    ) -> Result<Vec<uqa_sql::ast::ColumnDef>, SQLError> {
        self.event_analysis_context()
            .rule_relation_definitions(name)
    }
    fn rule_relation_columns_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, SQLError> {
        self.event_analysis_context()
            .rule_relation_column_names_with_control(name, control)
    }
    fn rule_relation_definitions_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<uqa_sql::ast::ColumnDef>>, SQLError> {
        self.event_analysis_context()
            .rule_relation_definitions_with_control(name, control)
    }
}
