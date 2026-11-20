//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored routine relation, column, and routine identities against fresh catalog inputs.

mod defaults;
mod progress;
use defaults::bind_default;
pub(crate) use defaults::bind_stored_parameter_default_from;
pub use progress::{RoutineDeclaration, RoutineDefinitionDependencies};

use super::compilation::RoutineCompilationContext;
use crate::{
    ast::{CreateFunction, FunctionBody},
    binding::{
        stored_columns::StoredSourceCatalog,
        stored_relations::bind_stored_statement_relations,
        stored_routines::{
            bind_catalog_statement_routines, bind_prepared_catalog_statement_routines,
            CatalogRoutineContext,
        },
    },
    catalog::{resolution::RelationLookupMode, stored_ast},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub use crate::binding::stored_routines::StoredCallOrigin as RoutineCompilationMode;

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
        Self::from_signature(context, def, mode, control)
    }

    fn from_signature(
        context: &RoutineCompilationContext<'_>,
        def: &CreateFunction,
        mode: RoutineCompilationMode,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
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

    async fn bind_from<R>(
        &self,
        catalog: &R,
        statement: &mut crate::ast::Statement,
        mode: RoutineCompilationMode,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<Produced<bool>, SQLError>
    where
        R: super::compilation::StoredRoutineCatalog
            + StoredSourceCatalog
            + super::resolution::RoutineOverloadCatalog
            + crate::plan::AggregateClassifier,
    {
        let positional = self.positional.as_slice();
        super::compilation::validate_sql_standard_statement_inputs(
            &RoutineCompilationContext::from_catalog(catalog),
            &super::resolution::RoutineOverloadContext { catalog },
            statement,
            positional,
            &self.parameters,
        )?;
        let mut changed = crate::binding::stored_relations::bind_stored_statement_relations_from(
            catalog,
            statement,
            RelationLookupMode::Bound,
            true,
            "SQL routine body",
            control,
            input,
        )
        .await?;
        changed |= super::merge_columns::bind_stored_merge_target_columns_from(
            catalog, statement, control, input,
        )
        .await?;
        changed |= catalog
            .stored_source_columns()
            .bind_statement_from(statement, control, input)
            .await?;
        let plan = crate::plan::UnifiedPlan::lower_executable_from_input(
            statement, catalog, control, input,
        )
        .await?
        .upgrade_stored_dispatches(control, input)
        .await?;
        let (plan, snapshot) = super::compilation::bind_stored_statement_from(
            catalog,
            plan,
            positional,
            &self.parameters,
            control,
            input,
        )
        .await?;
        let sites = crate::binding::stored_routines::statement_syntax_sites_from(
            catalog, plan, positional, &snapshot, control, input,
        )
        .await?;
        let (edited, memory) = mode
            .bind_statement_sites_with_work(statement, &sites, control, input)
            .await?
            .into_parts();
        Ok(control.finish(changed | edited, memory)?)
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
        changed |= mode.bind_statement_sites(statement, &routines.sites)?;
        Ok(changed)
    }
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
