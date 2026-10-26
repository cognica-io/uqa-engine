//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initialize live builtin calls before mutation triggers, CTE effects or row production.

use super::statement::context::MutationStatementContext;
use crate::{
    catalog::security::builtin_routines::execution::BuiltinRoutinePermissions,
    query::{binding::bind_source_plan_schema, privileges::routine_calls, CteScope},
    RowSchema,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    ast::ReturningAliases,
    catalog::roles::RoleReference,
    plan::{CtePlan, DeletePlan, InsertPlan, MergePlan, QueryPlan, SourcePlan, UpdatePlan},
    SQLError, SQLParam, ScalarExpr,
};

struct Inputs<'a> {
    table: &'a str,
    qualifier: &'a str,
    aliases: &'a ReturningAliases,
    ctes: &'a [CtePlan],
    subqueries: &'a [QueryPlan],
    source: Option<&'a SourcePlan>,
    input: Option<&'a QueryPlan>,
    expressions: Produced<Vec<&'a ScalarExpr>>,
    subject: Option<&'a RoleReference>,
    relations_bound: bool,
    insert: Option<&'a InsertPlan>,
}

impl<'a> Inputs<'a> {
    fn conflict_update_expressions(&self) -> impl Iterator<Item = &'a ScalarExpr> {
        self.insert
            .into_iter()
            .flat_map(InsertPlan::conflict_update_expressions)
    }

    fn has_conflict_update(&self) -> bool {
        self.conflict_update_expressions().next().is_some()
    }
}

pub fn insert<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &InsertPlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    initialize(
        context,
        |control| {
            Ok(Inputs {
                table: &statement.table,
                qualifier: &statement.target_qualifier,
                aliases: &statement.returning_aliases,
                ctes: &statement.ctes,
                subqueries: &statement.subqueries,
                source: None,
                input: statement.source.as_deref(),
                expressions: retained_expressions(control, statement.expressions(), None)?,
                subject: statement.statement_privilege_subject.as_ref(),
                relations_bound: statement.relations_bound,
                insert: Some(statement),
            })
        },
        params,
        inherited,
    )
}

pub fn update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &UpdatePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    initialize(
        context,
        |control| {
            Ok(Inputs {
                table: &statement.table,
                qualifier: &statement.target_qualifier,
                aliases: &statement.returning_aliases,
                ctes: &statement.ctes,
                subqueries: &statement.subqueries,
                source: statement.source.as_deref(),
                input: None,
                expressions: retained_expressions(
                    control,
                    statement.expressions(),
                    statement.source.as_deref(),
                )?,
                subject: statement.statement_privilege_subject.as_ref(),
                relations_bound: statement.relations_bound,
                insert: None,
            })
        },
        params,
        inherited,
    )
}

pub fn delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &DeletePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    initialize(
        context,
        |control| {
            Ok(Inputs {
                table: &statement.table,
                qualifier: &statement.target_qualifier,
                aliases: &statement.returning_aliases,
                ctes: &statement.ctes,
                subqueries: &statement.subqueries,
                source: statement.source.as_deref(),
                input: None,
                expressions: retained_expressions(
                    control,
                    statement.expressions(),
                    statement.source.as_deref(),
                )?,
                subject: statement.statement_privilege_subject.as_ref(),
                relations_bound: statement.relations_bound,
                insert: None,
            })
        },
        params,
        inherited,
    )
}

pub fn merge<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &MergePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    initialize(
        context,
        |control| {
            Ok(Inputs {
                table: &statement.target,
                qualifier: &statement.target_qualifier,
                aliases: &statement.returning_aliases,
                ctes: &statement.ctes,
                subqueries: &statement.subqueries,
                source: Some(&statement.source),
                input: None,
                expressions: retained_expressions(
                    control,
                    statement.expressions(),
                    Some(&statement.source),
                )?,
                subject: statement.statement_privilege_subject.as_ref(),
                relations_bound: false,
                insert: None,
            })
        },
        params,
        inherited,
    )
}

fn retained_expressions<'a>(
    control: ProductionControl<'_>,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    source: Option<&'a SourcePlan>,
) -> Result<Produced<Vec<&'a ScalarExpr>>, SQLError> {
    let mut retained = ProductionVec::new(control);
    for expression in expressions {
        retained.push_copy(expression)?;
    }
    if let Some(source) = source {
        source.try_visit_expressions(&mut |expression| {
            retained.push_copy(expression).map(|()| true)
        })?;
    }
    Ok(retained.finish()?)
}

fn initialize<'a, S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    inputs: impl FnOnce(ProductionControl<'_>) -> Result<Inputs<'a>, SQLError>,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(), SQLError> {
    let permissions = BuiltinRoutinePermissions::capture(&context.query.source.catalog);
    if permissions.unrestricted() {
        return Ok(());
    }
    let retained = context.query.source.ctes.queries.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.query.source.ctes.runtime.cancellation,
    );
    let inputs = inputs(control)?;
    let mut scope = context
        .mutation
        .scopes
        .command_scope(inputs.subject, inputs.relations_bound)?;
    if let Some(parent) = inherited {
        scope.inherit_cte_bindings(parent);
    }
    for cte in inputs.ctes {
        scope.insert_deferred(cte.clone());
    }
    scope.scalar_subqueries = inputs.subqueries.into();
    let mut visible = scope.enter_visible_ctes(inputs.ctes.iter().map(|cte| cte.name.as_str()));
    let scope = &mut *visible;
    if let Some(source) = inputs.source {
        crate::query::privileges::ensure_select_privileges_for_source_expressions(
            source,
            &inputs.expressions,
            scope,
        )?;
    }
    let (schema, conflict_schema) = expression_schemas(context, &inputs, params, scope)?;
    if let Some(input) = inputs.input {
        routine_calls::initialize(
            &context.query.source,
            &permissions,
            input,
            params,
            scope,
            None,
        )?;
    }
    let required = generated_columns(&inputs, scope)?;
    routine_calls::generated_relation_calls(
        &context.query.source,
        &permissions,
        required,
        params,
        scope,
    )?;
    for expression in inputs.expressions.iter() {
        let expression_schema = if inputs
            .conflict_update_expressions()
            .any(|other| std::ptr::eq(other, *expression))
        {
            conflict_schema.as_ref().unwrap_or(&schema)
        } else {
            &schema
        };
        routine_calls::expression_calls(
            &context.query.source,
            &permissions,
            expression,
            expression_schema,
            inputs.subqueries,
            params,
            scope,
        )?;
    }
    if let Some(source) = inputs.source {
        routine_calls::source_calls(
            &context.query.source,
            &permissions,
            source,
            params,
            scope,
            Some(&schema),
        )?;
    }
    Ok(())
}

fn expression_schemas<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    inputs: &Inputs<'_>,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<(RowSchema, Option<RowSchema>), SQLError> {
    let target = uqa_sql::semantics::returning::returning_target_schema(
        context.mutation.preparation.returning.catalog,
        inputs.table,
    )?;
    let source = inputs
        .source
        .map(|source| {
            bind_source_plan_schema(
                context.query.source.ctes.routines,
                source,
                params,
                scope,
                None,
            )
        })
        .transpose()?;
    let schema = uqa_sql::semantics::returning_expression_schema(
        &target,
        inputs.qualifier,
        inputs.aliases,
        source.as_ref(),
    );
    let conflict_schema = inputs.has_conflict_update().then(|| {
        let excluded = RowSchema::with_relation_qualifier(&target, "excluded");
        RowSchema::join(&schema, &excluded, std::iter::empty::<String>())
    });
    Ok((schema, conflict_schema))
}

fn generated_columns<S: Clone>(
    inputs: &Inputs<'_>,
    scope: &CteScope<S>,
) -> Result<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>, SQLError> {
    crate::query::privileges::with_scope(scope, |scope| {
        let mut qualifiers = std::collections::BTreeSet::from([
            inputs.table.to_string(),
            inputs.qualifier.to_string(),
            inputs.aliases.old.clone(),
            inputs.aliases.new.clone(),
        ]);
        if inputs.has_conflict_update() {
            qualifiers.insert("excluded".into());
        }
        let mut columns = uqa_sql::semantics::privileges::columns::for_target_expressions(
            inputs.table,
            &qualifiers,
            &inputs.expressions,
            scope,
        )?;
        if let Some(source) = inputs.source {
            for (table, required) in
                uqa_sql::semantics::privileges::columns::for_source_expressions(
                    source,
                    &inputs.expressions,
                    scope,
                )?
            {
                columns.entry(table).or_default().extend(required);
            }
        }
        Ok(columns)
    })
}

mod explain;
pub use explain::explain;

#[cfg(test)]
mod tests;
