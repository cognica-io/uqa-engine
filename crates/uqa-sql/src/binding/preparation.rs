//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation-time semantic analysis with ordered parameter coercion.

mod array_access;
mod collations;
pub(super) use collations::PendingCollations;
mod commands;
mod ctes;
mod explain;
mod expression_contexts;
mod expressions;
pub(super) use explain::read_generic_explain_inputs;
mod fields;
mod function_calls;
mod json_operators;
mod membership;
mod ownership;
mod parameters;
mod production;
pub(in crate::binding) use production::retain_input_metadata_owned;
pub use production::{
    bind_expression_plan_routines_for_storage_owned, resolve_variable_sites_owned,
};
pub use production::{
    bind_routine_parameter_references_owned, parameter_input_types_with_control,
    prepare_routine_body_inputs_owned, read_prepared_inputs_owned, read_procedural_inputs_owned,
    read_procedure_inputs_owned, retain_prepared_input_metadata_owned, PreparedOwnedInputs,
    PreparedOwnedRoutineBody, ProceduralOwnedInputs,
};
pub(in crate::binding) use production::{
    read_executable_inputs_owned, read_generic_explain_inputs_owned,
};
mod queries;
mod query_layouts;
#[cfg(test)]
pub(crate) use queries::BLOCK_ANALYSES;
pub use query_layouts::{restore_query_layouts, restore_query_layouts_owned};
mod routines;
mod rows;
pub(crate) use routines::{
    analyze_routine_body_argument, analyze_routine_body_inputs, prepare_procedure_call,
    prepare_routine_body_inputs, read_procedure_inputs,
};
mod schema_expressions;
mod set_contexts;
mod set_ordering;
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

pub(crate) struct ExecutableInputAnalysis {
    pub result: super::statements::AnalyzedResult,
    pub reusable: bool,
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
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants(
            declared,
            aliases,
            routines.enum_labels(),
            routines.catalog_input_functions(),
        )
        .retain_result_metadata(),
        schema_expression: None,
    };
    analysis.plan(plan)?;
    let constants = analysis.parameters.take_input_constants();
    let parameters = analysis.parameters.finish()?;
    let params = crate::prepared::analysis_parameters(&parameters);
    constants.apply(plan, routines, &params)?;
    retain_prepared_input_metadata(routines, plan, &parameters, ctes)?;
    let mut dependencies = analysis
        .scope
        .prepared_dependencies
        .take()
        .unwrap_or_default();
    plan.visit_scalar_expressions(&mut |expression| dependencies.include_expression(expression));
    Ok(PreparedInputAnalysis {
        parameter_types: parameters,
        dependencies,
    })
}

pub(crate) fn retain_prepared_input_metadata(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    parameters: &[Option<ColumnType>],
    binding: &BindingContext<'_>,
) -> Result<(), SQLError> {
    retain_compute_kinds(routines, plan);
    let params = crate::prepared::analysis_parameters(parameters);
    super::composite_inputs::retain_composite_inputs(routines, plan, &params, binding)?;
    plan.normalize_window_definitions()
}

/// Read ordinary statement inputs in prepared-analysis order and retain their
/// converted values in this executable plan. Runtime expressions stay deferred.
pub(crate) fn read_executable_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
) -> Result<ExecutableInputAnalysis, SQLError> {
    read_executable_inputs_inner(
        routines,
        plan,
        params,
        binding,
        aliases,
        InputAnalysisKind::Host,
    )
}

pub(crate) fn read_procedural_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
) -> Result<ExecutableInputAnalysis, SQLError> {
    read_executable_inputs_inner(
        routines,
        plan,
        params,
        binding,
        aliases,
        InputAnalysisKind::Procedural,
    )
}

enum InputAnalysisKind {
    Host,
    Procedural,
}

fn read_executable_inputs_inner(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[crate::SQLParam],
    binding: &BindingContext<'_>,
    aliases: &dyn OidAliasInput,
    kind: InputAnalysisKind,
) -> Result<ExecutableInputAnalysis, SQLError> {
    use super::statements::AnalyzedResult;

    let (declared, track_dependencies) = match kind {
        InputAnalysisKind::Host => (
            crate::params::bind_parameter_types(params, routines)?,
            false,
        ),
        InputAnalysisKind::Procedural => (super::statements::parameter_input_types(params)?, true),
    };
    let mut scope = SchemaScope::for_analysis(binding)?;
    scope.prepared_dependencies = track_dependencies.then(Default::default);
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut scope,
        parameters: ParameterTypes::with_input_constants(
            &declared,
            Some(aliases),
            routines.enum_labels(),
            routines.catalog_input_functions(),
        )
        .retain_result_metadata(),
        schema_expression: None,
    };
    let mut finish_parameters = false;
    let result = match &*plan {
        UnifiedPlan::Query(query) => AnalyzedResult::Schema(analysis.query(query, None)?),
        UnifiedPlan::Command(command) => match command.as_ref() {
            crate::plan::CommandPlan::CreateView { .. }
            | crate::plan::CommandPlan::CreateTableAs { .. }
            | crate::plan::CommandPlan::CreateMaterializedView { .. }
            | crate::plan::CommandPlan::DeclareCursor { .. } => {
                analysis.command(command)?;
                AnalyzedResult::Command
            }
            _ if command.mutation_target().is_some() => {
                finish_parameters = true;
                analysis
                    .command(command)?
                    .map_or(AnalyzedResult::Command, AnalyzedResult::Schema)
            }
            _ => AnalyzedResult::Command,
        },
    };
    let constants = analysis.parameters.take_input_constants();
    if finish_parameters {
        analysis.parameters.finish()?;
    }
    let reusable = constants.reusable_across_messages();
    constants.apply(plan, routines, params)?;
    retain_compute_kinds(routines, plan);
    super::composite_inputs::retain_composite_inputs(routines, plan, params, binding)?;
    plan.normalize_window_definitions()?;
    let mut dependencies = analysis
        .scope
        .prepared_dependencies
        .take()
        .unwrap_or_default();
    if track_dependencies {
        plan.visit_scalar_expressions(&mut |expression| {
            dependencies.include_expression(expression);
        });
    }
    Ok(ExecutableInputAnalysis {
        result,
        reusable,
        dependencies,
    })
}

pub fn infer_prepared_parameter_types(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut SchemaScope::for_analysis(ctes)?,
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

/// Analyze table CHECK inputs in expression order, including their system-column restrictions.
pub(crate) fn analyze_table_check(
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
        SchemaExpressionKind::TableCheck,
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
        bound_condition_queries: None,
        routines,
        scope: &mut SchemaScope::for_analysis(binding)?,
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
        .collations
        .validate_expression(&expression.scalar)?;
    analysis
        .parameters
        .take_input_constants()
        .apply_expression_with_subqueries(
            &mut expression.scalar,
            &mut expression.subqueries,
            routines,
            &[],
        )?;
    Ok(ty.map(ownership::AnalysisType::into_ordinary))
}

fn retain_compute_kinds(routines: &dyn RoutineResolution, plan: &mut UnifiedPlan) {
    let aggregates = |name: &str| routines.has_registered_aggregate_function(name);
    plan.visit_queries_mut(&mut |query| {
        if let crate::plan::RelationalPlan::QueryBlock(block) = &mut query.root {
            block.compute = block.compute_kind(&aggregates);
        }
    });
}

struct Preparation<'a> {
    routines: &'a dyn RoutineResolution,
    scope: &'a mut SchemaScope,
    parameters: ParameterTypes<'a>,
    schema_expression: Option<SchemaExpressionContext<'a>>,
    bound_condition_queries: Option<&'a super::type_resolution::BoundConditionQueries<'a>>,
}

/// Share expression analysis with ordinary output binding while borrowing its lexical scope and fixed parameters.
pub(super) fn projection_metadata(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    input: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[crate::SQLParam],
) -> Result<super::projection::ProjectionMetadata, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    let mut value = analysis.expression(expression, input, subqueries)?;
    analysis
        .parameters
        .coerce_unknown(&mut value, &ColumnType::Text)?;
    if matches!(value.ty.as_deref(), Some(ColumnType::Composite(_))) {
        value.record_fields = analysis
            .scope
            .bind_record_fields(routines, expression, input, subqueries, params)?;
    }
    analysis
        .scope
        .collations
        .append(analysis.parameters.collations);
    let collation = value
        .ty
        .as_deref()
        .filter(|ty| crate::schema::composites::type_is_collatable(ty))
        .map(|_| u32::try_from(value.derivation.oid()).expect("selected collation is an OID"));
    let record = value.record_metadata();
    Ok(super::projection::ProjectionMetadata {
        ty: value
            .ty
            .map(|ty| ty.into_produced(&analysis.scope.control()))
            .transpose()?,
        collation,
        record,
    })
}

/// Record input collations and type-observer selections on already bound stored calls, as analysis records them for an executable statement. Constants and routine identities stay unchanged, while definitions without another analysis pass retain the complete selected metadata.
pub(crate) fn assign_stored_input_collations(
    routines: &dyn RoutineResolution,
    expression: &mut crate::plan::ExpressionPlan,
    input: &RowSchema,
    binding: &BindingContext<'_>,
) -> Result<(), SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut SchemaScope::for_analysis(binding)?,
        parameters: ParameterTypes::fixed(&[]).with_call_collations(),
        schema_expression: None,
    };
    analysis.expression(&expression.scalar, input, &expression.subqueries)?;
    analysis
        .parameters
        .take_call_collations()
        .apply(&mut expression.scalar, &mut expression.subqueries)
}

/// Restore input collations and type-observer selections on already bound stored queries without rereading constants or changing routine identities.
pub fn assign_stored_query_input_collations(
    routines: &dyn RoutineResolution,
    query: &mut QueryPlan,
    binding: &BindingContext<'_>,
) -> Result<(), SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        scope: &mut SchemaScope::for_analysis(binding)?,
        parameters: ParameterTypes::fixed(&[]).with_call_collations(),
        schema_expression: None,
    };
    analysis.query(query, None)?;
    analysis
        .parameters
        .take_call_collations()
        .apply_query(query)
}

/// The input collation of a call whose binding is selected after statement analysis, such as a table function in `FROM`, derived from its arguments as analysis derives every other call's.
pub(super) fn call_input_collation(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    name: &str,
    args: &[ScalarExpr],
    input: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[crate::SQLParam],
) -> Result<Option<crate::catalog::collations::CollationSelection>, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    let arguments = analysis.observe_function_arguments(name, args, &[], input, subqueries)?;
    arguments.input_collation(args.len(), None, false)
}

pub(super) fn condition_metadata(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    construct: &str,
    params: &[crate::SQLParam],
    queries: &super::type_resolution::BoundConditionQueries<'_>,
) -> Result<(), SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: Some(queries),
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    let placement = match construct {
        "WHERE" => crate::semantics::sets::placement::SetPlacement::Where,
        "HAVING" => crate::semantics::sets::placement::SetPlacement::Having,
        "JOIN/ON" => crate::semantics::sets::placement::SetPlacement::JoinCondition,
        _ => analysis.scope.set_placement,
    };
    analysis.in_placement(placement, |analysis| {
        analysis.boolean_expression(expression, queries.input, queries.plans, construct)
    })?;
    analysis
        .scope
        .collations
        .append(analysis.parameters.collations);
    Ok(())
}

/// VALUES relation typing and collation selection share preparation's row-then-column order.
pub(super) fn values_metadata(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    outer: Option<&RowSchema>,
    params: &[crate::SQLParam],
) -> Result<RowSchema, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    analysis
        .values(rows, subqueries, outer)
        .and_then(|output| output.schema(&analysis.scope.control()))
}

/// Keep each set operand's original output observations until common type and collation selection finish.
pub(super) fn set_metadata(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    root: &crate::plan::RelationalPlan,
    outer: Option<&RowSchema>,
    params: &[crate::SQLParam],
) -> Result<RowSchema, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    let output = analysis.root(root, outer, false)?;
    analysis
        .scope
        .collations
        .append(analysis.parameters.collations);
    output.schema(&analysis.scope.control())
}

/// Ordinary query blocks retain the same original output observations through clause and key selection.
pub(super) fn block_metadata(
    scope: &mut SchemaScope,
    routines: &dyn RoutineResolution,
    block: &crate::plan::QueryBlockPlan,
    source: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[crate::SQLParam],
) -> Result<RowSchema, SQLError> {
    let mut analysis = Preparation {
        bound_condition_queries: None,
        routines,
        parameters: ParameterTypes::fixed_with_control(params, &scope.control())?,
        scope,
        schema_expression: None,
    };
    let output = analysis.block_with_source(block, source, outer, false)?;
    analysis
        .scope
        .collations
        .append(analysis.parameters.collations);
    output.schema(&analysis.scope.control())
}

struct QueryOutput {
    columns: ownership::AnalysisVec<uqa_core::memory::Produced<String>>,
    types: ownership::AnalysisVec<ExpressionType>,
    open: bool,
    /// Sparse original targets for indeterminate output collations; addresses are never dereferenced.
    key_origins: ownership::AnalysisVec<(usize, std::ptr::NonNull<ScalarExpr>)>,
    /// Leaf output names of this set-operation query, retained only for inaccessible-column diagnostics.
    set_sources: ownership::AnalysisVec<ownership::AnalysisVec<uqa_core::memory::Produced<String>>>,
    control: uqa_core::memory::OwnedProductionControl,
}

impl QueryOutput {
    fn new(control: &uqa_core::memory::ProductionControl<'_>) -> Self {
        Self {
            columns: ownership::AnalysisVec::new(control),
            types: ownership::AnalysisVec::new(control),
            open: false,
            key_origins: ownership::AnalysisVec::new(control),
            set_sources: ownership::AnalysisVec::new(control),
            control: control.capture(),
        }
    }

    fn key_origin(&self, position: usize) -> Option<std::ptr::NonNull<ScalarExpr>> {
        self.key_origins
            .binary_search_by_key(&position, |(position, _)| *position)
            .ok()
            .map(|index| self.key_origins[index].1)
    }

    fn schema(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        RowSchema::with_output_metadata_with_control(
            None,
            self.columns.iter().zip(&self.types).map(|(name, value)| {
                // Unknown targets keep their original collation until coercion selects a collatable output type.
                let ty = value.ty.as_deref();
                let collation = ty
                    .filter(|ty| crate::schema::composites::type_is_collatable(ty))
                    .map(|_| {
                        u32::try_from(value.derivation.oid()).expect("selected collation is an OID")
                    });
                (name.as_str(), ty, collation, value.record_metadata())
            }),
            self.open,
            control,
        )
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
    ) -> Result<Option<ownership::AnalysisType>, SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let defer_windows = self.schema_expression.is_none()
            && crate::semantics::windows::expr_has_window(expression);
        let canonical = if self.scope.routine_parameters.is_some() || defer_windows {
            Some(expression.copy_for_rewrite(&control)?.transform_nodes(
                &control,
                &mut |_, node| {
                    if let Some(parameter) = self.scope.routine_parameter_reference(&node, input) {
                        return Ok(ScalarExpr::Param(parameter).copy_for_rewrite(&control)?);
                    }
                    if defer_windows {
                        if let ScalarExpr::WindowCall { spec, .. } = &*node {
                            let spec = crate::ScalarWindowSpec {
                                definition: spec.definition,
                                partition_by: Vec::new(),
                                order_by: Vec::new(),
                                frame: None,
                            };
                            return Ok(node.replace_window_spec(&spec, &control)?);
                        }
                    }
                    Ok(node)
                },
            )?)
        } else {
            None
        };
        let expression = canonical.as_deref().unwrap_or(expression);
        self.scope
            .bind_expression_type_with_control(
                self.routines,
                expression,
                input,
                subqueries,
                &self.parameters.values()?,
            )?
            .map(|ty| ownership::AnalysisType::from_produced(ty, &control))
            .transpose()
    }

    fn type_name(&self, name: &str) -> Result<ownership::AnalysisType, SQLError> {
        let control = self.scope.control();
        let ty = self
            .routines
            .resolve_type_name_with_control(name, &control)?
            .map_or_else(
                || ColumnType::from_sql_name_with_control(name, &control),
                Ok,
            )?;
        ownership::AnalysisType::from_produced(ty, &control)
    }

    fn require_boolean(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        context: &str,
    ) -> Result<(), SQLError> {
        self.boolean_expression(expression, input, subqueries, context)
            .map(|_| ())
    }

    /// Analyze an operand a construct requires to be boolean, as `coerce_to_boolean` does: its type first, then whether it returns a set.
    fn boolean_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        context: &str,
    ) -> Result<ExpressionType, SQLError> {
        let calls = self.set_calls();
        let mut observed = self.expression(expression, input, subqueries)?;
        self.parameters
            .coerce_unknown(&mut observed, &ColumnType::Boolean)?;
        crate::type_resolution::boolean::require_argument_type(
            observed.ty.as_deref(),
            context,
            Some(self.routines),
        )?;
        if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = expression {
            crate::expr::cast_value(value, "boolean")?;
        }
        self.reject_set_inputs(calls, || {
            crate::semantics::sets::placement::set_argument_error(context)
        })?;
        Ok(observed)
    }
}
