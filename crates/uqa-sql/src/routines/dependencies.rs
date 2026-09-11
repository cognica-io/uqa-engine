//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored routine relation, column, and routine identities against fresh catalog inputs.

use super::{compilation::RoutineCompilationContext, CompiledFunctionBody};
use crate::{
    ast::{CreateFunction, FunctionBody},
    binding::{
        bind_expression_plan_routines_for_storage,
        stored_columns::StoredSourceCatalog,
        stored_relations::bind_stored_statement_relations,
        stored_routines::{
            bind_catalog_statement_routines, collect_expression_routine_references,
            CatalogRoutineContext,
        },
    },
    catalog::{resolution::RelationLookupMode, stored_ast},
    plan::ExpressionPlan,
    RowSchema, SQLError,
};

#[derive(Clone, Copy)]
pub enum RoutineCompilationMode {
    Definition,
    Persisted,
}

pub fn bind_routine_definition_dependencies(
    context: &RoutineCompilationContext<'_>,
    sources: &dyn StoredSourceCatalog,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    let mut changed = bind_sql_standard_body_relations(context, sources, def, mode)?;
    for parameter in &mut def.params {
        let Some(default) = &mut parameter.default else {
            continue;
        };
        let mut plan = ExpressionPlan::lower_with(default.clone(), &|name: &str| {
            context.catalog.has_registered_aggregate_function(name)
        });
        let binding = context.catalog.binding_snapshot()?;
        bind_expression_plan_routines_for_storage(
            context.routines,
            &mut plan,
            &[],
            &binding.context(),
            &RowSchema::default(),
        )?;
        let references = collect_expression_routine_references(&plan)?;
        changed |= stored_ast::bind_stored_expression_routines(default, &references)?;
    }
    Ok(changed)
}

fn bind_sql_standard_body_relations(
    context: &RoutineCompilationContext<'_>,
    sources: &dyn StoredSourceCatalog,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    let FunctionBody::Statements(statements) = &mut def.body else {
        return Ok(false);
    };
    let mut changed = false;
    for statement in statements {
        changed |= bind_stored_statement_relations(
            context.relations,
            statement,
            RelationLookupMode::Dynamic,
            matches!(mode, RoutineCompilationMode::Persisted),
            "SQL routine body",
        )?;
        changed |=
            super::merge_columns::bind_stored_merge_target_columns(context.merge, statement)?;
        changed |= sources.stored_source_columns().bind_statement(statement)?;
    }
    Ok(changed)
}

pub fn bind_sql_standard_body_routines(
    context: &RoutineCompilationContext<'_>,
    def: &mut CreateFunction,
    compiled: &CompiledFunctionBody,
) -> Result<bool, SQLError> {
    let FunctionBody::Statements(statements) = &mut def.body else {
        return Ok(false);
    };
    let CompiledFunctionBody::SQL(plans) = compiled else {
        return Err(SQLError::Internal(format!(
            "SQL-standard routine `{}` did not compile to SQL plans",
            def.name
        )));
    };
    if statements.len() != plans.len() {
        return Err(SQLError::Internal(format!(
            "SQL-standard routine `{}` has {} statements but {} plans",
            def.name,
            statements.len(),
            plans.len()
        )));
    }
    let mut changed = false;
    for (statement, plan) in statements.iter_mut().zip(plans) {
        let binding = context.catalog.binding_snapshot()?;
        let routines = bind_catalog_statement_routines(
            &CatalogRoutineContext {
                routines: context.routines,
                binding: &binding.context(),
            },
            plan,
        )?;
        changed |= stored_ast::bind_stored_statement_routines(statement, &routines.references)?;
    }
    Ok(changed)
}
