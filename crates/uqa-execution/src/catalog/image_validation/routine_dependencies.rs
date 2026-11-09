//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join concrete routine references from one native catalog without compiling deferred bodies.

use crate::catalog::CatalogReadSnapshot;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{
    ast::{ColumnDef, FunctionBody},
    plan::PlanRef,
    routines::{native::NativeRoutineReferences, CompiledFunctionBody, RoutineBody},
    SQLError,
};

pub fn validate(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let references = NativeRoutineReferences::new(
        d.sql_user_functions
            .values()
            .flatten()
            .map(|function| &function.def),
        memory,
        cancellation,
    )?;
    for table in snapshot.tables.values() {
        cancellation.check()?;
        columns(&references, &table.columns, cancellation)?;
        for check in table.checks.iter() {
            references.expression(&check.expr)?;
        }
        if let Some(spec) = &table.hierarchy.partition_spec {
            for key in &spec.keys {
                references.expression(key)?;
            }
        }
    }
    for table in d.foreign_tables.values() {
        cancellation.check()?;
        columns(&references, &table.columns, cancellation)?;
        for check in &table.checks {
            references.expression(&check.expr)?;
        }
    }
    for domain in d.domains.values() {
        cancellation.check()?;
        if let Some(default) = &domain.definition.default {
            references.expression(default)?;
        }
        for check in &domain.definition.checks {
            references.expression(&check.expression)?;
        }
    }
    for view in d.views.values() {
        references.plan(PlanRef::Query(&view.query))?;
    }
    for function in d.sql_user_functions.values().flatten() {
        cancellation.check()?;
        for parameter in &function.def.params {
            cancellation.check()?;
            if let Some(default) = &parameter.default {
                references.expression(default)?;
            }
        }
        if let FunctionBody::Statements(statements) = &function.def.body {
            for statement in statements {
                references.statement(statement)?;
            }
        }
        if let RoutineBody::Bound(body) = &function.body {
            let CompiledFunctionBody::SQL(plans) = &***body else {
                return Err(SQLError::Internal(
                    "native catalog contains a session routine body".into(),
                ));
            };
            for plan in plans {
                references.plan(plan.into())?;
            }
        }
    }
    for triggers in d.triggers.values() {
        cancellation.check()?;
        for trigger in triggers.values() {
            cancellation.check()?;
            if let Some(identity) = trigger.function_object_id {
                references.identity(identity, &[], false)?;
            }
            if let Some(condition) = &trigger.definition.when {
                references.expression(condition)?;
            }
        }
    }
    validate_rules(&d.rules, &references, cancellation)?;
    cancellation.check()?;
    Ok(())
}

fn validate_rules(
    catalog: &uqa_sql::catalog::events::RuleCatalog,
    references: &NativeRoutineReferences<'_>,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    for rules in catalog.values() {
        cancellation.check()?;
        for rule in rules.values() {
            cancellation.check()?;
            for action in &rule.definition.actions {
                references.statement(action)?;
            }
            if let Some(dependencies) = &rule.dependencies {
                for routine in &dependencies.routines {
                    cancellation.check()?;
                    if let Some(identity) = routine.object_id {
                        references.identity(identity, &routine.argument_types, false)?;
                    }
                }
            }
            if let Some(plan) = &rule.condition_plan {
                references.scalar(&plan.scalar)?;
                for query in &plan.subqueries {
                    references.plan(PlanRef::Query(query))?;
                }
            }
        }
    }
    Ok(())
}

fn columns(
    references: &NativeRoutineReferences<'_>,
    columns: &[ColumnDef],
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    for column in columns {
        cancellation.check()?;
        for expression in column.default.iter().chain(&column.check) {
            references.expression(expression)?;
        }
        if let Some(generated) = &column.generated {
            references.expression(&generated.expression)?;
            for binding in &generated.function_dependencies {
                references.binding(binding)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
