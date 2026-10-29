//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule declaration and RETURNING contracts.

use crate::{
    ast::{RuleEvent, Statement},
    catalog::events::StoredRule,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_core::RelationIdentity;

/// Resolve stored rule definitions and a mutation's canonical target.
pub trait RuleCatalog {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError>;
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError>;
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError>;
    fn resolve_mutation_target(&self, name: &str, bound: bool) -> Result<String, SQLError>;

    /// External catalogs may supply owned metadata. Native catalogs override this observation to lend the selected immutable rule declarations directly.
    fn visit_rules_with_control(
        &self,
        table: &str,
        event: RuleEvent,
        reverse: bool,
        control: &ProductionControl<'_>,
        visit: &mut dyn FnMut(&StoredRule) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let rules = self.rules_for(table, event)?;
        if reverse {
            for rule in rules.iter().rev() {
                control.check()?;
                visit(rule)?;
            }
        } else {
            for rule in &rules {
                control.check()?;
                visit(rule)?;
            }
        }
        Ok(())
    }

    fn resolve_rule_relation_with_control(
        &self,
        table: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationIdentity>, SQLError> {
        control.check()?;
        let relation = self.resolve_rule_relation(table)?;
        production::copy_relation(&relation, control)
    }

    fn resolve_mutation_target_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        control.check()?;
        Ok(control.copy_text(&self.resolve_mutation_target(name, bound)?)?)
    }
}

pub mod binding;
mod ctes;
mod input_contract;
mod production;
pub use ctes::validate_rule_cte_contract;
pub use input_contract::validate_insert_rule_query_contract;
pub use production::{
    validate_insert_rule_query_contract_with_control, validate_rule_cte_contract_with_control,
    validate_rule_returning_contract_with_control,
};
pub fn validate_rule_returning_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    requested: bool,
) -> Result<(), SQLError> {
    validate_rule_returning_contract_with_control(
        catalog,
        table,
        event,
        requested,
        &ProductionControl::uncontrolled(),
    )
}

const fn rule_event_name(event: RuleEvent) -> &'static str {
    match event {
        RuleEvent::Select => "SELECT",
        RuleEvent::Insert => "INSERT",
        RuleEvent::Update => "UPDATE",
        RuleEvent::Delete => "DELETE",
    }
}

pub fn statement_has_returning(statement: &Statement) -> bool {
    match statement {
        Statement::Insert(statement) => !statement.returning.is_empty(),
        Statement::Update(statement) => !statement.returning.is_empty(),
        Statement::Delete(statement) => !statement.returning.is_empty(),
        _ => false,
    }
}

pub fn clear_statement_returning(statement: &mut Statement) {
    match statement {
        Statement::Insert(statement) => statement.returning.clear(),
        Statement::Update(statement) => statement.returning.clear(),
        Statement::Delete(statement) => statement.returning.clear(),
        _ => {}
    }
}

pub mod action_binding;

pub mod analysis;
pub mod returning;

pub mod insert_inputs;
