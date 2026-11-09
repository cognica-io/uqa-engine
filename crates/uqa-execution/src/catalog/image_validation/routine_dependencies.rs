//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join concrete routine references from one native catalog without compiling deferred bodies.

use crate::catalog::CatalogReadSnapshot;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::{ColumnDef, Expr, FunctionBinding, FunctionBody, Statement},
    plan::PlanRef,
    routines::{native::NativeRoutineReferences, CompiledFunctionBody, RoutineBody},
    SQLError, ScalarExpr,
};

/// One stored-expression inventory shared by native reference predicates. Deferred source text is never compiled by this traversal.
pub(super) trait References {
    fn expression(&self, expression: &Expr) -> Result<(), SQLError>;
    fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError>;
    fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError>;
    fn statement(&self, statement: &Statement) -> Result<(), SQLError>;
    fn binding(&self, binding: &FunctionBinding) -> Result<(), SQLError>;
    fn identity(
        &self,
        identity: [u8; 16],
        arguments: &[String],
        procedure: bool,
    ) -> Result<(), SQLError>;
}

impl References for NativeRoutineReferences<'_> {
    fn expression(&self, expression: &Expr) -> Result<(), SQLError> {
        self.expression(expression)
    }
    fn scalar(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        self.scalar(expression)
    }
    fn plan(&self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        self.plan(plan)
    }
    fn statement(&self, statement: &Statement) -> Result<(), SQLError> {
        self.statement(statement)
    }
    fn binding(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.binding(binding)
    }
    fn identity(
        &self,
        identity: [u8; 16],
        arguments: &[String],
        procedure: bool,
    ) -> Result<(), SQLError> {
        self.identity(identity, arguments, procedure)
    }
}

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
    visit(
        snapshot,
        &references,
        &ProductionControl::new(memory, cancellation, cancellation),
    )
}

pub(super) fn visit(
    snapshot: &CatalogReadSnapshot,
    references: &impl References,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    for table in snapshot.tables.values() {
        control.check()?;
        columns(references, &table.columns, control)?;
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
        control.check()?;
        columns(references, &table.columns, control)?;
        for check in &table.checks {
            references.expression(&check.expr)?;
        }
    }
    for domain in d.domains.values() {
        control.check()?;
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
        control.check()?;
        for parameter in &function.def.params {
            control.check()?;
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
        control.check()?;
        for trigger in triggers.values() {
            control.check()?;
            if let Some(identity) = trigger.function_object_id {
                references.identity(identity, &[], false)?;
            }
            if let Some(condition) = &trigger.definition.when {
                references.expression(condition)?;
            }
        }
    }
    validate_rules(&d.rules, references, control)?;
    control.check()?;
    Ok(())
}

fn validate_rules(
    catalog: &uqa_sql::catalog::events::RuleCatalog,
    references: &impl References,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for rules in catalog.values() {
        control.check()?;
        for rule in rules.values() {
            control.check()?;
            for action in &rule.definition.actions {
                references.statement(action)?;
            }
            if let Some(dependencies) = &rule.dependencies {
                for routine in &dependencies.routines {
                    control.check()?;
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
    references: &impl References,
    columns: &[ColumnDef],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for column in columns {
        control.check()?;
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
