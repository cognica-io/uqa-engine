//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared registration ordered around retained session registry publication.

pub mod invalidation;

use std::sync::Arc;
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl};
use uqa_sql::{
    plan::{AggregateClassifier, MutableExecutablePlan, UnifiedPlan},
    prepared::{definition::PreparedDefinitionContext, entry::PreparedStatementPlan},
    ColumnType, SQLError, Statement,
};

pub trait PreparedDefinitionWrite {
    fn insert(
        &mut self,
        name: Produced<String>,
        definition: PreparedStatementPlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError>;
}

pub trait PreparedDefinitionRegistry {
    fn write_definitions(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Box<dyn PreparedDefinitionWrite + '_>, SQLError>;
    fn production_resources(&self) -> Result<OwnedProductionControl, SQLError> {
        Ok(ProductionControl::uncontrolled().capture())
    }
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
    let owner = context.registry.production_resources()?;
    let control = owner.control();
    let plan = UnifiedPlan::lower_borrowed_for_rewrite(&statement, context.aggregates, &control)?;
    register_definition(context, name, plan, &[], None, false, &control)
}

pub fn register_plan(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    logical_plan: UnifiedPlan,
    declared: &[ColumnType],
    source_sql: Option<&str>,
) -> Result<(), SQLError> {
    let owner = context.registry.production_resources()?;
    let control = owner.control();
    register_definition(
        context,
        name,
        logical_plan.copy_for_rewrite(&control)?,
        declared,
        source_sql,
        source_sql.is_some(),
        &control,
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
    let owner = context.registry.production_resources()?;
    let control = owner.control();
    let plan = UnifiedPlan::lower_borrowed_for_rewrite(statement, context.aggregates, &control)?;
    register_definition(context, name, plan, declared, Some(sql), false, &control)
}

fn register_definition(
    context: &PreparedRegistrationContext<'_>,
    name: String,
    logical_plan: MutableExecutablePlan,
    declared: &[ColumnType],
    source_sql: Option<&str>,
    from_sql: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let _statement_thread = super::thread_context::StatementThreadContext {
        retained_rows: crate::query::sources::retained_source::RetainedRowContext::current(),
        io: uqa_core::io_usage::IOContext::current(),
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
    let source_plan = logical_plan.finish(control)?.into_shared(control)?;
    let definition = uqa_sql::prepared::definition::analyze_definition_owned(
        &context.analysis,
        source_plan.copy_for_rewrite(control)?,
        declared,
        control,
    )?
    .into_shared_with_control(control)?;
    let name = control.copy_text(&name)?;
    let source_sql = source_sql
        .map(|sql| {
            uqa_sql::prepared::ownership::SharedPreparedInput::from_produced(
                control.copy_text(sql)?,
                control,
            )
        })
        .transpose()?;
    context.registry.write_definitions(control)?.insert(
        name,
        PreparedStatementPlan {
            definition_identity: std::sync::Arc::new(()),
            composite_inputs: definition.composite_inputs,
            source_plan,
            logical_plan: definition.logical_plan,
            needs_analysis: false,
            effective_search_path: definition.effective_search_path,
            dependencies: definition.dependencies,
            dependency_snapshot: definition.dependency_snapshot,
            plan: None,
            parameter_types: definition.parameter_types,
            result_schema: definition.result_schema,
            source_sql,
            prepared_at_micros: (context.clock)(),
            from_sql,
            generic_plans: 0,
            custom_plans: 0,
            generic_cost: None,
            total_custom_cost: 0.0,
        },
        control,
    )
}

#[cfg(test)]
mod tests;
