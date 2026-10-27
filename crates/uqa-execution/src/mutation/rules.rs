//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute independent rewrite-rule queries with ordered actions and scoped recursion.
use std::cell::RefCell;
use uqa_core::RelationIdentity;
use uqa_sql::semantics::rules::analysis::rule_returning_columns;
use uqa_sql::{ast::RuleEvent, SQLError};
mod context;
mod mutation_inputs;
mod query_inputs;
mod returning;
pub mod schedule;
pub use context::{RuleContext, RuleSecurity, RuleStatements, ViewRuleContext};
pub use mutation_inputs::execute_mutation_query_rules;
pub use query_inputs::run_insert_query_rules;
pub use returning::{RuleOutputProjection, RuleReturningProjection, RuleReturningResult};
pub use uqa_sql::semantics::rules::{
    returning::RuleReturningRequest, validate_rule_returning_contract,
};
thread_local! {
    static RULE_EXECUTION_STACK: RefCell<Vec<(String, RuleEvent)>> = const { RefCell::new(Vec::new()) };
}
#[derive(Default)]
pub struct RuleExecutionOutcome {
    pub returning: Option<RuleReturningResult>,
    pub affected_rows: u64,
    pub sets_command_tag: bool,
}

struct RuleExecutionGuard {
    table: String,
    event: RuleEvent,
}

impl RuleExecutionGuard {
    fn enter(table: &str, event: RuleEvent) -> Result<Self, SQLError> {
        ensure_not_recursive(table, event)?;
        RULE_EXECUTION_STACK.with(|stack| stack.borrow_mut().push((table.to_string(), event)));
        Ok(Self {
            table: table.to_string(),
            event,
        })
    }
}

impl Drop for RuleExecutionGuard {
    fn drop(&mut self) {
        RULE_EXECUTION_STACK.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some((self.table.clone(), self.event)));
        });
    }
}

fn ensure_not_recursive(table: &str, event: RuleEvent) -> Result<(), SQLError> {
    let recursive = RULE_EXECUTION_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .any(|(active_table, active_event)| active_table == table && *active_event == event)
    });
    if !recursive {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{table}`: {error}")))?;
    Err(SQLError::Routine {
        sqlstate: "42P17".into(),
        message: format!(
            "infinite recursion detected in rules for relation \"{}\"",
            relation.name
        ),
    })
}
