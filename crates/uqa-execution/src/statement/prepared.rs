//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared registration ordered around retained session registry publication.

pub mod invalidation;

use std::sync::Arc;
use uqa_sql::{
    plan::{AggregateClassifier, UnifiedPlan},
    prepared::{definition::PreparedDefinitionContext, entry::PreparedStatementPlan},
    ColumnType, SQLError, Statement,
};

pub trait PreparedDefinitionWrite {
    fn insert(&mut self, name: String, definition: PreparedStatementPlan);
}

pub trait PreparedDefinitionRegistry {
    fn write_definitions(&self) -> Box<dyn PreparedDefinitionWrite + '_>;
}

#[derive(Clone, Copy)]
pub struct PreparedRegistrationContext<'a> {
    pub analysis: PreparedDefinitionContext<'a>,
    pub aggregates: &'a dyn AggregateClassifier,
    pub registry: &'a dyn PreparedDefinitionRegistry,
    /// The session's active transaction clock, including direct host registration; `None` keeps the caller's clock.
    pub transaction_timestamp_micros: Option<i64>,
    pub date_order: Option<uqa_core::TemporalDateOrder>,
    pub interval_style: Option<uqa_core::TemporalIntervalStyle>,
    /// The session's cancellation, which the definition's analysis observes.
    pub cancellation: &'a uqa_core::CancellationToken,
    pub clock: fn() -> i64,
}

pub fn register_statement(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    statement: Statement,
) -> Result<(), SQLError> {
    let plan = UnifiedPlan::lower_with(statement, context.aggregates);
    register_plan(context, name, plan, &[], None)
}

pub fn register_plan(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    logical_plan: UnifiedPlan,
    declared: &[ColumnType],
    source_sql: Option<&str>,
) -> Result<(), SQLError> {
    register_definition(
        context,
        name,
        logical_plan,
        declared,
        source_sql,
        source_sql.is_some(),
    )
}

/// Parse a prepared message before selecting its analysis snapshot. Empty input remains an explicit native definition.
pub fn parse_host_sql(
    sql: &str,
    settings: uqa_sql::parser::ParserSettings,
    notices: &crate::query::NoticeQueue,
    cancellation: &uqa_core::CancellationToken,
) -> Result<Arc<Statement>, SQLError> {
    cancellation.check()?;
    let (statements, parser) =
        uqa_sql::parser::with_settings(settings, || uqa_sql::parse_statements(sql));
    for notice in parser.notices.iter() {
        notices.push(notice.clone());
    }
    let mut statements = statements?;
    if statements.len() > 1 {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "cannot insert multiple commands into a prepared statement".into(),
        });
    }
    let statement = statements
        .pop()
        .map(|statement| statement.compile())
        .transpose()?
        .unwrap_or(Statement::Empty);
    Ok(Arc::new(statement))
}

/// Analyze the parsed host definition in its admitted session boundary. Retain source text without marking it as SQL PREPARE.
pub fn register_host_statement(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    statement: &Statement,
    sql: &str,
    declared: &[ColumnType],
) -> Result<(), SQLError> {
    let plan = UnifiedPlan::lower_with(statement.clone(), context.aggregates);
    register_definition(context, name, plan, declared, Some(sql), false)
}

fn register_definition(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    logical_plan: UnifiedPlan,
    declared: &[ColumnType],
    source_sql: Option<&str>,
    from_sql: bool,
) -> Result<(), SQLError> {
    let _statement_thread = super::thread_context::StatementThreadContext {
        retained_rows: crate::query::sources::retained_source::RetainedRowContext::current(),
        regex_cache: uqa_sql::expr::current_regex_cache(),
        temporal_input: uqa_sql::expr::TemporalInputContext {
            transaction_clock_micros: context
                .transaction_timestamp_micros
                .or_else(uqa_sql::expr::transaction_clock_micros),
            interval_style: context
                .interval_style
                .unwrap_or_else(uqa_sql::expr::temporal_interval_style),
            date_order: context
                .date_order
                .unwrap_or_else(uqa_sql::expr::temporal_date_order),
        },
        cancellation: Some(context.cancellation.clone()),
    }
    .enter();
    let source_plan = Arc::new(logical_plan.clone());
    let definition = uqa_sql::prepared::definition::analyze_definition(
        &context.analysis,
        logical_plan,
        declared,
    )?;
    context.registry.write_definitions().insert(
        name,
        PreparedStatementPlan {
            composite_inputs: definition.composite_inputs,
            source_plan,
            logical_plan: Arc::new(definition.logical_plan),
            needs_analysis: false,
            effective_search_path: definition.effective_search_path,
            dependencies: definition.dependencies,
            dependency_snapshot: definition.dependency_snapshot,
            plan: None,
            parameter_types: definition.parameter_types,
            result_schema: definition.result_schema,
            source_sql: source_sql.map(Arc::from),
            prepared_at_micros: (context.clock)(),
            from_sql,
            generic_plans: 0,
            custom_plans: 0,
            generic_cost: None,
            total_custom_cost: 0.0,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests;
