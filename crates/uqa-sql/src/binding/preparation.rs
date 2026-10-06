//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation-time semantic analysis with ordered parameter coercion.

mod commands;
mod ctes;
mod expression_contexts;
mod expressions;
mod function_calls;
mod parameters;
mod queries;
mod routines;
pub(crate) use routines::{
    analyze_routine_body_argument, analyze_routine_body_inputs, prepare_procedure_call,
    prepare_routine_body_inputs,
};
mod schema_expressions;
mod sources;
#[cfg(test)]
mod tests;

use super::{BindingContext, RowSchema, SchemaScope};
use crate::plan::{QueryPlan, UnifiedPlan};
use crate::routines::RoutineResolution;
use crate::schema::dependencies::oid_alias::OidAliasInput;
use crate::ScalarExpr;
use crate::{ColumnType, SQLError};
use parameters::{error, ExpressionType, ParameterTypes};
use schema_expressions::{SchemaExpressionContext, SchemaExpressionKind};

#[derive(Debug)]
pub(crate) struct PreparedInputAnalysis {
    pub parameter_types: Vec<Option<ColumnType>>,
    pub dependencies: crate::prepared::dependencies::PreparedAnalysisDependencies,
}

/// Read input constants while the prepared definition's original tree stays in place. The short-lived literal identities never escape this operation; only converted values enter the stored plan.
pub(crate) fn read_prepared_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &BindingContext,
    aliases: Option<&dyn OidAliasInput>,
) -> Result<PreparedInputAnalysis, SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.prepared_dependencies =
        Some(crate::prepared::dependencies::PreparedAnalysisDependencies::default());
    let mut analysis = Preparation {
        routines,
        scope,
        parameters: ParameterTypes::with_input_constants(
            declared,
            aliases,
            routines.enum_labels(),
            routines.catalog_input_functions(),
        ),
        schema_expression: None,
    };
    analysis.plan(plan)?;
    let constants = analysis.parameters.take_input_constants();
    let parameters = analysis.parameters.finish()?;
    constants.apply(plan)?;
    plan.normalize_window_definitions()?;
    let mut dependencies = analysis.scope.prepared_dependencies.unwrap_or_default();
    plan.visit_scalar_expressions(&mut |expression| dependencies.include_expression(expression));
    Ok(PreparedInputAnalysis {
        parameter_types: parameters,
        dependencies,
    })
}

/// Read ordinary statement inputs in prepared-analysis order and retain their
/// converted values in this executable plan. Runtime expressions stay deferred.
pub(crate) fn read_executable_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
) -> Result<bool, SQLError> {
    read_executable_inputs_inner(routines, plan, params, binding, aliases, false)
        .map(|(reusable, _)| reusable)
}

pub(crate) fn read_procedural_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
) -> Result<crate::prepared::dependencies::PreparedAnalysisDependencies, SQLError> {
    read_executable_inputs_inner(routines, plan, params, binding, aliases, true)
        .map(|(_, dependencies)| dependencies)
}

fn read_executable_inputs_inner(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
    track_dependencies: bool,
) -> Result<
    (
        bool,
        crate::prepared::dependencies::PreparedAnalysisDependencies,
    ),
    SQLError,
> {
    let declared = super::statements::parameter_input_types(params)?;
    let mut scope = SchemaScope::for_analysis(binding)?;
    scope.prepared_dependencies = track_dependencies.then(Default::default);
    let mut analysis = Preparation {
        routines,
        scope,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
        ),
        schema_expression: None,
    };
    let mut finish_parameters = false;
    match &*plan {
        UnifiedPlan::Query(query) => {
            analysis.query(query, None)?;
        }
        UnifiedPlan::Command(command) => match command.as_ref() {
            crate::plan::CommandPlan::CreateView { query, .. }
            | crate::plan::CommandPlan::CreateTableAs { query, .. }
            | crate::plan::CommandPlan::CreateMaterializedView { query, .. }
            | crate::plan::CommandPlan::DeclareCursor { query, .. } => {
                analysis.query(query, None)?;
            }
            _ if command.mutation_target().is_some() => {
                analysis.command(command)?;
                finish_parameters = true;
            }
            _ => {}
        },
    }
    let constants = analysis.parameters.take_input_constants();
    if finish_parameters {
        analysis.parameters.finish()?;
    }
    let reusable = constants.reusable_across_messages();
    constants.apply(plan)?;
    plan.normalize_window_definitions()?;
    let mut dependencies = analysis.scope.prepared_dependencies.unwrap_or_default();
    if track_dependencies {
        plan.visit_scalar_expressions(&mut |expression| {
            dependencies.include_expression(expression);
        });
    }
    Ok((reusable, dependencies))
}

pub fn infer_prepared_parameter_types(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(ctes)?,
        parameters: ParameterTypes::new(declared),
        schema_expression: None,
    };
    analysis.plan(plan)?;
    analysis.parameters.finish()
}

/// Analyze a USING expression against the original table row type before the ALTER target or its new type is checked. Transform expressions admit no parameters or query-valued descendants; input functions run during analysis and runtime expressions stay deferred.
pub fn analyze_column_type_transform(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    expression: &mut crate::plan::ExpressionPlan,
    input: &RowSchema,
    binding: &BindingContext<'_>,
) -> Result<Option<ColumnType>, SQLError> {
    analyze_schema_expression(
        catalog,
        expression,
        input,
        binding,
        SchemaExpressionKind::TypeTransform,
    )
}

/// Analyze the new domain CHECK against VALUE's base type. This uses the same ordered expression analysis as ordinary SQL and rejects query, aggregate, window and set-valued expressions at their own analysis boundary.
pub(crate) fn analyze_domain_check(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    expression: &mut crate::plan::ExpressionPlan,
    input: &RowSchema,
    binding: &BindingContext<'_>,
) -> Result<Option<ColumnType>, SQLError> {
    analyze_schema_expression(
        catalog,
        expression,
        input,
        binding,
        SchemaExpressionKind::DomainCheck,
    )
}

fn analyze_schema_expression(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    expression: &mut crate::plan::ExpressionPlan,
    input: &RowSchema,
    binding: &BindingContext<'_>,
    kind: SchemaExpressionKind,
) -> Result<Option<ColumnType>, SQLError> {
    analyze_expression_inputs(
        catalog,
        catalog,
        expression,
        input,
        binding,
        Some(SchemaExpressionContext {
            aggregates: catalog,
            kind,
        }),
    )
}

/// Retain input literals in typed stored syntax before routine binding and publication.
pub(crate) fn analyze_stored_expression_inputs(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    expression: &mut crate::plan::ExpressionPlan,
    binding: &BindingContext<'_>,
    input: &RowSchema,
) -> Result<Option<ColumnType>, SQLError> {
    analyze_expression_inputs(catalog, catalog, expression, input, binding, None)
}

/// Analyze and freeze input literals in a DEFAULT with no parameter or row namespace. Expression restrictions apply in `PostgreSQL`'s traversal order.
pub(crate) fn analyze_default_inputs(
    routines: &dyn RoutineResolution,
    aggregates: &dyn crate::plan::AggregateClassifier,
    aliases: &dyn OidAliasInput,
    expression: &mut crate::plan::ExpressionPlan,
    binding: &BindingContext<'_>,
) -> Result<Option<ColumnType>, SQLError> {
    analyze_expression_inputs(
        routines,
        aliases,
        expression,
        &RowSchema::default(),
        binding,
        Some(SchemaExpressionContext {
            aggregates,
            kind: SchemaExpressionKind::Default,
        }),
    )
}

fn analyze_expression_inputs<'a>(
    routines: &'a dyn RoutineResolution,
    aliases: &'a dyn OidAliasInput,
    expression: &mut crate::plan::ExpressionPlan,
    input: &RowSchema,
    binding: &BindingContext<'_>,
    schema_expression: Option<SchemaExpressionContext<'a>>,
) -> Result<Option<ColumnType>, SQLError> {
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(binding)?,
        parameters: ParameterTypes::with_input_constants(
            &[],
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
        ),
        schema_expression,
    };
    let ty = analysis
        .expression(&expression.scalar, input, &expression.subqueries)?
        .ty;
    analysis
        .parameters
        .take_input_constants()
        .apply_expression(&mut expression.scalar)?;
    Ok(ty)
}

struct Preparation<'a> {
    routines: &'a dyn RoutineResolution,
    scope: SchemaScope,
    parameters: ParameterTypes<'a>,
    schema_expression: Option<SchemaExpressionContext<'a>>,
}

struct QueryOutput {
    columns: Vec<String>,
    types: Vec<ExpressionType>,
    open: bool,
}

impl QueryOutput {
    fn schema(&self) -> RowSchema {
        let schema =
            RowSchema::with_types(
                self.columns.clone(),
                self.types.iter().map(|value| value.ty.clone()).collect(),
            )
            .with_record_fields(self.types.iter().enumerate().filter_map(
                |(index, value)| value.record_fields.clone().map(|fields| (index, fields)),
            ));
        if self.open {
            RowSchema::with_open_columns(&schema, None)
        } else {
            schema
        }
    }
}

impl Preparation<'_> {
    fn plan(&mut self, plan: &UnifiedPlan) -> Result<(), SQLError> {
        match plan {
            UnifiedPlan::Query(query) => {
                self.query(query, None)?;
            }
            UnifiedPlan::Command(command) => {
                self.command(command)?;
            }
        }
        Ok(())
    }

    fn known_type(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
        let mut canonical;
        let defer_windows = self.schema_expression.is_none()
            && crate::semantics::windows::expr_has_window(expression);
        let expression = if self.scope.routine_parameters.is_some() || defer_windows {
            canonical = expression.clone();
            self.scope
                .canonicalize_routine_parameters(&mut canonical, input);
            if defer_windows {
                // Window functions bind their arguments with the target list;
                // their specifications are transformed after the query clauses.
                canonical.visit_mut(&mut |node| {
                    if let ScalarExpr::WindowCall { spec, .. } = node {
                        spec.partition_by.clear();
                        spec.order_by.clear();
                        spec.frame = None;
                    }
                });
            }
            &canonical
        } else {
            expression
        };
        self.scope.bind_expression_type(
            self.routines,
            expression,
            input,
            subqueries,
            &self.parameters.values(),
            Some(input),
        )
    }

    fn type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        self.routines
            .resolve_type_name(name)?
            .map_or_else(|| ColumnType::from_sql_name(name), Ok)
    }

    fn require_boolean(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        context: &str,
    ) -> Result<(), SQLError> {
        let mut observed = self.expression(expression, input, subqueries)?;
        self.parameters
            .coerce_unknown(&mut observed, &ColumnType::Boolean)?;
        let Some(mut ty) = observed.ty.as_ref() else {
            return Ok(());
        };
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
        }
        if !matches!(ty, ColumnType::Boolean) {
            return Err(error(
                "42804",
                format!(
                    "argument of {context} must be type boolean, not type {}",
                    ty.regtype_name()
                ),
            ));
        }
        if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = expression {
            crate::expr::cast_value(value, "boolean")?;
        }
        Ok(())
    }
}
