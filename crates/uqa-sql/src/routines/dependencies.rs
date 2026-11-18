//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored routine relation, column, and routine identities against fresh catalog inputs.

mod progress;
pub use progress::{RoutineDeclaration, RoutineDefinitionDependencies};

use super::compilation::RoutineCompilationContext;
use crate::{
    ast::{CreateFunction, FunctionBody},
    binding::{
        bind_expression_plan_routines_for_storage,
        stored_columns::StoredSourceCatalog,
        stored_relations::bind_stored_statement_relations,
        stored_routines::{
            bind_catalog_statement_routines, bind_prepared_catalog_statement_routines,
            CatalogRoutineContext,
        },
        syntax_sites::expression_syntax_sites,
    },
    catalog::{resolution::RelationLookupMode, stored_ast},
    plan::ExpressionPlan,
    RowSchema, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[derive(Clone, Copy)]
pub enum RoutineCompilationMode {
    Definition,
    Persisted,
}

pub fn bind_routine_definition_dependencies(
    context: &RoutineCompilationContext<'_>,
    sources: &dyn StoredSourceCatalog,
    overloads: &super::resolution::RoutineOverloadContext<'_>,
    def: &mut CreateFunction,
    mode: RoutineCompilationMode,
) -> Result<bool, SQLError> {
    let mut binding = RoutineDefinitionDependencies::new(mode);
    let control = ProductionControl::uncontrolled();
    loop {
        if let Some(changed) = binding.advance(context, sources, overloads, def, &control)? {
            return Ok(changed);
        }
    }
}

fn bind_default(
    context: &RoutineCompilationContext<'_>,
    parameter: &mut crate::ast::FunctionParam,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let Some(default) = &mut parameter.default else {
        return Ok(false);
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
        changed |=
            stored_ast::fold_assigned_stored_literal(default, &ty, context.routines.enum_labels())?;
    }
    Ok(changed)
}

/// Bind a copy of every statement of a SQL-standard body and carry the bound routine identities, user-defined type identities and enum constants back into the stored statements, then convert the result literals the final statement assigns to the declared result. A name resolves to a parameter only when no column of its statement takes it, as `sql_fn_post_column_ref` resolves it; the final statement's result is checked when the body is validated.
struct SQLBodyInputs {
    positional: Produced<Vec<crate::SQLParam>>,
    parameters: crate::binding::RoutineParameterScope,
    result_types: Produced<Vec<Option<crate::ColumnType>>>,
    recording: Option<super::body_parameters::ParameterRecording>,
}

impl SQLBodyInputs {
    fn new(
        context: &RoutineCompilationContext<'_>,
        def: &CreateFunction,
        mode: RoutineCompilationMode,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        super::compilation::validate_routine_signature(context, def)?;
        let positional = super::body_validation::routine_parameter_values_with_control(
            context.types,
            def,
            control,
        )?;
        let parameters = super::body_parameters::sql_body_parameter_scope_with_control(
            def,
            &positional,
            control,
        )?;
        let result_types = def_result_types(context, &def.params, &def.returns, control)?;
        let recording = matches!(mode, RoutineCompilationMode::Definition)
            .then(|| super::body_parameters::ParameterRecording::new(context, def))
            .transpose()?;
        Ok(Self {
            positional,
            parameters,
            result_types,
            recording,
        })
    }

    fn bind(
        &self,
        context: &RoutineCompilationContext<'_>,
        sources: &dyn StoredSourceCatalog,
        overloads: &super::resolution::RoutineOverloadContext<'_>,
        statement: &mut crate::ast::Statement,
        mode: RoutineCompilationMode,
    ) -> Result<bool, SQLError> {
        let positional = self.positional.as_slice();
        let parameters = &self.parameters;
        let lowering = super::compilation::SQLRoutineLowering {
            bind_catalog_dependencies: true,
            persisted_definition: matches!(mode, RoutineCompilationMode::Persisted),
            preserve_target_expressions: true,
        };
        let mut changed = false;
        super::compilation::validate_sql_standard_statement_inputs(
            context, overloads, statement, positional, parameters,
        )?;
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
        if let Some(recording) = &self.recording {
            changed |= recording.record(context, statement)?;
        }
        let mut lowered =
            super::compilation::lower_sql_routine_statement(context, statement.clone(), lowering)?;
        let binding = context.catalog.binding_snapshot()?;
        crate::binding::bind_routine_parameter_references(
            context.routines,
            &mut lowered,
            positional,
            &binding.context(),
            parameters,
        )?;
        let routines = CatalogRoutineContext {
            routines: context.routines,
            binding: &binding.context(),
        };
        let routines = match mode {
            RoutineCompilationMode::Definition => {
                // Fresh syntax completes input/target analysis before the next statement. Retain its original sites when storing converted constants.
                let mut prepared = lowered.clone();
                crate::binding::prepare_routine_body_inputs(
                    context.routines,
                    &mut prepared,
                    positional,
                    routines.binding,
                    context.catalog,
                    None,
                )?;
                bind_prepared_catalog_statement_routines(
                    &routines, &lowered, &prepared, positional,
                )?
            }
            // A stored definition keeps analyzed targets, including dropped MERGE attributes whose expressions remain dependencies. Execution normalizes its own copy against live target identities.
            RoutineCompilationMode::Persisted => {
                bind_catalog_statement_routines(&routines, &lowered, positional)?
            }
        };
        changed |= stored_ast::bind_stored_statement_sites(statement, &routines.sites)?;
        Ok(changed)
    }
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
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<crate::ColumnType>>>, SQLError> {
    use crate::ast::{FunctionParamMode, FunctionReturns};
    let mut result = ProductionVec::new(*control);
    let mut push = |name: &str| -> Result<(), SQLError> {
        let ty = if crate::ast::UserTypeIdentity::parse(name).is_some() {
            context
                .routines
                .resolve_type_name_with_control(name, control)?
        } else {
            None
        };
        let (ty, memory) = ty.map_or_else(
            || (None, control.empty_reservation()),
            |ty| {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            },
        );
        result.push_produced(control.finish(ty, memory)?)?;
        Ok(())
    };
    let mut outputs = false;
    for parameter in params {
        if matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
        ) {
            push(&parameter.type_name)?;
            outputs = true;
        }
    }
    if !outputs {
        if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
            returns
        {
            push(type_name)?;
        }
    }
    Ok(result.finish()?)
}

/// `check_sql_fn_retval` coerces each result column of the final statement to its declared type; an `unknown` literal selected directly becomes a constant of that type.
fn fold_sql_function_result(
    context: &RoutineCompilationContext<'_>,
    types: &[Option<crate::ColumnType>],
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
    for (projection, ty) in select.projections.iter_mut().zip(types) {
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
