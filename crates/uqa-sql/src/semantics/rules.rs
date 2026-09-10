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
use uqa_core::RelationIdentity;

/// Resolve stored rule definitions and a mutation's canonical target.
pub trait RuleCatalog {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError>;
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError>;
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError>;
    fn resolve_mutation_target(&self, name: &str, bound: bool) -> Result<String, SQLError>;
}

pub mod binding;
pub fn validate_rule_returning_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    requested: bool,
) -> Result<(), SQLError> {
    if !requested {
        return Ok(());
    }
    let table = catalog.resolve_rule_relation(table)?.qualified_name();
    let rules = catalog.rules_for(&table, event)?;
    if rules.is_empty() || !rules.iter().any(|rule| rule.definition.instead) {
        return Ok(());
    }
    let providers = rules
        .iter()
        .flat_map(|rule| &rule.definition.actions)
        .filter(|action| statement_has_returning(action))
        .count();
    if providers > 1 {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    if providers == 1 {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(&table)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{table}`: {error}")))?;
    let event = rule_event_name(event);
    Err(SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot perform {event} RETURNING on relation \"{}\"",
            relation.name
        ),
        detail: None,
        hint: Some(format!(
            "You need an unconditional ON {event} DO INSTEAD rule with a RETURNING clause."
        )),
    })
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
