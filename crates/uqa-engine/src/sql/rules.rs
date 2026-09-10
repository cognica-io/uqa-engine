//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::Value;
pub(in crate::sql) use uqa_execution::mutation::rules::{
    validate_rule_returning_contract, PreparedRuleBatch, RuleReturningRequest, RuleRowImage,
    RuleRowSide,
};
use uqa_sql::{ast::RuleEvent, SQLError};
pub(in crate::sql) fn relation_suppresses_original_query(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
        engine.rule_analysis_context(),
        table,
        event,
    )
}

pub(in crate::sql) fn relation_rules_require_event_rows(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::rules::analysis::relation_rules_require_event_rows(
        engine.rule_analysis_context(),
        table,
        event,
    )
}
pub(in crate::sql) fn surviving_view_rules_require_event_rows(
    engine: &Engine,
    relations: &[String],
    event: RuleEvent,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::rules::analysis::surviving_view_rules_require_event_rows(
        engine.rule_analysis_context(),
        relations,
        event,
    )
}
pub(in crate::sql) fn relation_condition_row_columns(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<BTreeSet<String>, SQLError> {
    uqa_sql::semantics::rules::analysis::relation_condition_row_columns(
        engine.rule_analysis_context(),
        table,
        event,
    )
}
pub(in crate::sql) fn relation_rule_row_columns(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    uqa_sql::semantics::rules::analysis::relation_rule_row_columns(
        engine.rule_analysis_context(),
        table,
        event,
    )
}
pub(in crate::sql) fn prepare_rule_batch(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
) -> Result<PreparedRuleBatch, SQLError> {
    uqa_execution::mutation::rules::prepare_rule_batch(
        engine.rule_execution_context(),
        table,
        event,
        rows,
    )
}
pub(in crate::sql) fn prepare_rule_batch_with_projection<F>(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
    project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    uqa_execution::mutation::rules::prepare_rule_batch_with_projection(
        engine.rule_execution_context(),
        table,
        event,
        rows,
        project,
    )
}
