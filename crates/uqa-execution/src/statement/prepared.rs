//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared registration ordered around retained session registry publication.

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
    let definition = uqa_sql::prepared::definition::analyze_definition(
        &context.analysis,
        logical_plan,
        declared,
    )?;
    context.registry.write_definitions().insert(
        name,
        PreparedStatementPlan {
            logical_plan: Arc::new(definition.logical_plan),
            plan: None,
            parameter_types: definition.parameter_types,
            result_schema: definition.result_schema,
            source_sql: source_sql.map(Arc::from),
            prepared_at_micros: (context.clock)(),
            from_sql: source_sql.is_some(),
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
