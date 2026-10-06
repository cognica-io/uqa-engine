//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored routine relation, column, and routine identities against fresh catalog inputs.

use super::compilation::RoutineCompilationContext;
use crate::{
    ast::{CreateFunction, FunctionBody},
    binding::{
        bind_expression_plan_routines_for_storage,
        stored_columns::StoredSourceCatalog,
        stored_relations::bind_stored_statement_relations,
        stored_routines::{bind_catalog_statement_routines, CatalogRoutineContext},
        syntax_sites::expression_syntax_sites,
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
        let lowered = ExpressionPlan::lower_with(default.clone(), &|name: &str| {
            context.catalog.has_registered_aggregate_function(name)
        });
        let mut plan = lowered.clone();
        let binding = context.catalog.binding_snapshot()?;
        let default_type = bind_expression_plan_routines_for_storage(
            context.routines,
            &mut plan,
            &[],
            &binding.context(),
            &RowSchema::default(),
        )?;
        let default_type =
            if crate::type_resolution::routine_polymorphic_type(&parameter.type_name).is_some() {
                default_type
            } else {
                Some(
                    context
                        .types
                        .resolve_catalog_column_type_name(&parameter.type_name)?
                        .without_type_modifiers(),
                )
            };
        let default_type =
            super::defaults::default_expression_type(&parameter.type_name, default, default_type);
        if parameter.default_type != default_type {
            parameter.default_type = default_type;
            changed = true;
        }
        let sites = expression_syntax_sites(&lowered, &plan)?;
        changed |= stored_ast::bind_stored_expression_sites(default, &sites)?;
        // A default is assigned to its parameter.
        if let Some(ty) = user_defined_type(context, &parameter.type_name)? {
            changed |= stored_ast::fold_assigned_stored_literal(
                default,
                &ty,
                context.routines.enum_labels(),
            )?;
        }
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
        super::compilation::validate_sql_standard_statement(statement)?;
        changed |= bind_stored_statement_relations(
            context.relations,
            statement,
            match mode {
                RoutineCompilationMode::Definition => RelationLookupMode::Dynamic,
                RoutineCompilationMode::Persisted => RelationLookupMode::Bound,
            },
            matches!(mode, RoutineCompilationMode::Persisted),
            "SQL routine body",
        )?;
        changed |=
            super::merge_columns::bind_stored_merge_target_columns(context.merge, statement)?;
        changed |= sources.stored_source_columns().bind_statement(statement)?;
    }
    Ok(changed)
}

/// Bind a copy of every statement of a SQL-standard body and carry the bound routine identities, user-defined type identities and enum constants back into the stored statements, then convert the result literals the final statement assigns to the declared result. A name resolves to a parameter only when no column of its statement takes it, as `sql_fn_post_column_ref` resolves it; the final statement's result is checked when the body is validated.
pub fn bind_sql_standard_body_routines(
    context: &RoutineCompilationContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    if !matches!(def.body, FunctionBody::Statements(_)) {
        return Ok(false);
    }
    // Declaration errors precede body input functions, which can execute domain checks.
    super::compilation::validate_routine_signature(context, def)?;
    let positional = super::body_validation::routine_parameter_values(context.types, def);
    let parameters = super::body_parameters::sql_body_parameter_scope(def, &positional)?;
    let result_types = def_result_types(context, &def.params, &def.returns)?;
    let FunctionBody::Statements(statements) = &mut def.body else {
        return Ok(false);
    };
    let lowering = super::compilation::SQLRoutineLowering {
        bind_catalog_dependencies: true,
        persisted_definition: matches!(mode, RoutineCompilationMode::Persisted),
        preserve_target_expressions: true,
    };
    let mut changed = false;
    for statement in statements.iter_mut() {
        let mut lowered =
            super::compilation::lower_sql_routine_statement(context, statement.clone(), lowering)?;
        let binding = context.catalog.binding_snapshot()?;
        crate::binding::bind_routine_parameter_references(
            context.routines,
            &mut lowered,
            &positional,
            &binding.context(),
            &parameters,
        )?;
        let routines = bind_catalog_statement_routines(
            &CatalogRoutineContext {
                routines: context.routines,
                binding: &binding.context(),
            },
            &lowered,
            &positional,
        )?;
        changed |= stored_ast::bind_stored_statement_sites(statement, &routines.sites)?;
    }
    if let Some(statement) = statements.last_mut() {
        changed |= fold_sql_function_result(context, result_types, statement)?;
    }
    Ok(changed)
}

/// A declared routine type that is user-defined, the only kind whose `unknown` literals parse analysis converts to stored enum constants. Declarations name user-defined types by identity.
fn user_defined_type(
    context: &RoutineCompilationContext<'_>,
    name: &str,
) -> Result<Option<crate::ColumnType>, SQLError> {
    if crate::ast::UserTypeIdentity::parse(name).is_none() {
        return Ok(None);
    }
    context.routines.resolve_type_name(name)
}

/// The column types a SQL function's final statement is coerced to: the `OUT` and `TABLE` parameters, or else the declared result. Built-in and pseudo-type columns are `None`.
fn def_result_types(
    context: &RoutineCompilationContext<'_>,
    params: &[crate::ast::FunctionParam],
    returns: &crate::ast::FunctionReturns,
) -> Result<Vec<Option<crate::ColumnType>>, SQLError> {
    use crate::ast::{FunctionParamMode, FunctionReturns};
    let outputs = params
        .iter()
        .filter(|parameter| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
        })
        .collect::<Vec<_>>();
    let names = if outputs.is_empty() {
        match returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                vec![type_name.as_str()]
            }
            FunctionReturns::None | FunctionReturns::Table => Vec::new(),
        }
    } else {
        outputs
            .iter()
            .map(|parameter| parameter.type_name.as_str())
            .collect()
    };
    names
        .into_iter()
        .map(|name| user_defined_type(context, name))
        .collect()
}

/// `check_sql_fn_retval` coerces each result column of the final statement to its declared type; an `unknown` literal selected directly becomes a constant of that type.
fn fold_sql_function_result(
    context: &RoutineCompilationContext<'_>,
    types: Vec<Option<crate::ColumnType>>,
    statement: &mut crate::ast::Statement,
) -> Result<bool, SQLError> {
    let crate::ast::Statement::Select(select) = statement else {
        return Ok(false);
    };
    if types.is_empty()
        || select.set_op.is_some()
        || !select.values.is_empty()
        || select.projections.len() != types.len()
    {
        return Ok(false);
    }
    let mut changed = false;
    for (projection, ty) in select.projections.iter_mut().zip(&types) {
        let Some(ty) = ty else {
            continue;
        };
        changed |= stored_ast::fold_assigned_stored_literal(
            &mut projection.expr,
            ty,
            context.routines.enum_labels(),
        )?;
    }
    Ok(changed)
}
