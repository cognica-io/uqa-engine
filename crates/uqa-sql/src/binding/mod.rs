//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static relational row-type binding.
//!
//! This module derives source and query output schemas from plans, catalog
//! declarations, and already-bound CTE schemas. It never executes a query or
//! samples a result row, so empty, spilled, and correlated relations retain
//! the same declared `PostgreSQL` type identities as non-empty relations.

mod analysis;
pub mod catalog_sources;
mod command_scopes;
mod commands;
pub mod composite_dependencies;
mod composite_inputs;
pub mod composite_rename;
mod cte_controls;
mod ctes;
mod dependencies;
mod execution;
pub mod legacy_collations;
pub mod legacy_patterns;
mod merge_scopes;
mod ordered_calls;
mod preparation;
mod projection;
mod record_fields;
mod routine_binding;
mod routine_parameters;
mod scope;
pub mod scoped_references;
mod source_schemas;
mod sources;
pub use sources::bind_source_plan_schema_in_scope;
pub mod statements;
mod type_resolution;
mod variable_sites;

pub use commands::analyze_prepared_command_schema;
pub use execution::{
    bind_query_block_for_execution, bind_query_block_with_sources, BoundExecutionBlock,
    ExecutionSourceBindings,
};
pub use preparation::{
    analyze_column_type_transform, assign_stored_query_input_collations,
    infer_prepared_parameter_types, restore_query_layouts,
};
pub(crate) use preparation::{
    analyze_default_inputs, analyze_routine_body_argument, analyze_routine_body_inputs,
    prepare_routine_body_inputs, read_procedure_inputs,
};
pub(crate) use preparation::{
    analyze_domain_check, analyze_stored_expression_inputs, analyze_table_check,
    assign_stored_input_collations, read_prepared_inputs, retain_prepared_input_metadata,
};

#[cfg(test)]
mod tests;

pub use cte_controls::{analyze_recursive_control_step, extend_cte_generated_schema};
pub use projection::{
    analyze_projection_output_schema, bind_projection_output_schema,
    validate_query_block_expression_types, validate_query_block_references,
};
pub use routine_binding::{
    bind_expression_plan_routines_for_storage, bind_query_plan_routines_for_storage,
    bind_syntax_query_plan_routines,
};
pub use routine_parameters::{bind_routine_parameter_references, RoutineParameterScope};
pub use scope::SourceScope;
pub use scope::{
    analyze_expression_plan_type, analyze_query_plan_schema,
    analyze_query_plan_schema_with_catalog, bind_expression_plan_type, bind_query_plan_schema,
};
pub use scope::{overlay_outer_schema, values_schema_in_scope, values_types_in_scope};
pub use sources::{
    analyze_source_plan_schema, bind_source_input_schema, bind_source_plan_schema,
    bind_source_plan_schema_for_execution, with_query_table_pseudo_columns, SourceBindingInput,
};
pub use variable_sites::{resolve_variable_sites, VariableSiteResolution};

use catalog_sources::operator_join_relation_schemas;
use cte_controls::extend_recursive_cte_binding_schema;
pub use cte_controls::hide_recursive_generated_schema;
use projection::{projection_star_columns, rename_schema};
use sources::{alias_table_schema, table_function_member_source, JoinSchemaBinding};
use type_resolution::QueryFunctionTypeResolver;

use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::semantics::{cte_references_own_name, expr_contains_subquery, projection_columns};
use crate::{SQLError, SQLParam, ScalarExpr};
use catalog_sources::user_function_output_columns;
pub use context::BindingContext;

pub mod context;
mod rule_inputs;
use crate::ast::ColumnType;
use crate::catalog::analysis::CatalogReadView;
use crate::catalog::resolution::RelationNameResolution;
use crate::routines::RoutineResolution;
use crate::semantics::sets::placement_scope::SetPlacementScope;
use crate::semantics::{
    apply_table_function_aliases, table_function_column_types, table_function_empty_schema,
    validate_table_function_alias_count, validate_table_function_column_definition,
    TableFunctionTypeRequest,
};
use crate::RowSchema;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[cfg(test)]
thread_local! {
    static SOURCE_BINDINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy)]
enum ScalarBindingMode {
    References,
    Stored,
    CompositeInputs,
}

struct SchemaScope {
    execution_sources: Option<execution::CapturedSources>,
    collations: preparation::PendingCollations,
    catalog: CatalogReadView,
    resolution: RelationNameResolution,
    ctes: BTreeMap<String, RowSchema>,
    deferred_ctes: BTreeMap<String, Arc<crate::plan::CtePlan>>,
    non_returning_ctes: BTreeSet<String>,
    visiting_views: BTreeSet<String>,
    validate_references: bool,
    stored_expression_outer: Option<RowSchema>,
    rule_condition_event: Option<crate::ast::RuleEvent>,
    /// Native MERGE action syntax is legal in a RETURNING expression and its descendant queries.
    merge_returning: bool,
    /// The kind of expression under analysis at this query level, which decides where a set-returning call may appear, as `PostgreSQL`'s `p_expr_kind` does.
    set_placement: crate::semantics::sets::placement::SetPlacement,
    /// The set-returning calls analyzed so far at this query level. Two readings differ exactly when an expression analyzed between them contains one, as `PostgreSQL` compares `p_last_srf`.
    set_calls: usize,
    stored_whole_rows: BTreeMap<String, ScalarExpr>,
    /// The parameters of the SQL routine whose body is bound, as the outermost scope: a reference that resolves into them, because no column of any query level takes its name, becomes the positional parameter it names.
    routine_parameters: Option<RoutineParameterScope>,
    /// Select reference resolution, complete stored binding, or prepared composite identity retention.
    scalar_binding: ScalarBindingMode,
    /// The names of a `PL/pgSQL` statement that the function's variables take, checked against what the statement can see.
    variable_sites: Option<variable_sites::VariableSites>,
    /// Keep `*` projections and `GROUP BY` output-name references as written, so a bound copy of stored syntax still corresponds to that syntax node for node.
    preserve_syntax_shape: bool,
    prepared_dependencies: Option<crate::prepared::dependencies::PreparedAnalysisDependencies>,
    composite_dependencies: Option<Vec<crate::catalog::dependencies::ObjectAddress>>,
}

fn non_returning_cte_error(name: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "WITH query \"{name}\" does not have a RETURNING clause"
    ))
}

impl SchemaScope {
    fn from_context(ctes: &BindingContext) -> Result<Self, SQLError> {
        Ok(Self {
            execution_sources: None,
            collations: preparation::PendingCollations::default(),
            catalog: ctes.catalog.clone(),
            resolution: ctes.resolution.clone(),
            ctes: ctes.ctes.clone(),
            deferred_ctes: ctes.deferred_ctes.clone(),
            non_returning_ctes: ctes.non_returning_ctes.clone(),
            visiting_views: BTreeSet::new(),
            validate_references: false,
            stored_expression_outer: None,
            rule_condition_event: None,
            merge_returning: false,
            set_placement: crate::semantics::sets::placement::SetPlacement::Expanding,
            set_calls: 0,
            stored_whole_rows: BTreeMap::new(),
            routine_parameters: None,
            scalar_binding: ScalarBindingMode::Stored,
            variable_sites: None,
            preserve_syntax_shape: false,
            prepared_dependencies: None,
            composite_dependencies: None,
        })
    }

    fn for_analysis(ctes: &BindingContext) -> Result<Self, SQLError> {
        let mut scope = Self::from_context(ctes)?;
        scope.validate_references = true;
        Ok(scope)
    }

    fn for_catalog_analysis(catalog: CatalogReadView, resolution: RelationNameResolution) -> Self {
        Self {
            execution_sources: None,
            collations: preparation::PendingCollations::default(),
            catalog,
            resolution,
            ctes: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: BTreeSet::new(),
            visiting_views: BTreeSet::new(),
            validate_references: true,
            stored_expression_outer: None,
            rule_condition_event: None,
            merge_returning: false,
            set_placement: crate::semantics::sets::placement::SetPlacement::Expanding,
            set_calls: 0,
            stored_whole_rows: BTreeMap::new(),
            routine_parameters: None,
            scalar_binding: ScalarBindingMode::Stored,
            variable_sites: None,
            preserve_syntax_shape: false,
            prepared_dependencies: None,
            composite_dependencies: None,
        }
    }

    fn bind_query(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let lookup_mode = if plan.relations_bound {
            crate::catalog::resolution::RelationLookupMode::Bound
        } else {
            crate::catalog::resolution::RelationLookupMode::Dynamic
        };
        let previous = self.resolution.set_lookup_mode(lookup_mode);
        let result = self.bind_query_inner(routines, plan, params, outer);
        self.resolution.set_lookup_mode(previous);
        result
    }

    fn bind_query_inner(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let previous = self.bind_cte_schemas(routines, &plan.ctes, params, outer)?;

        let result = self.bind_root(routines, &plan.root, params, outer);
        self.restore_cte_schemas(previous);
        let output = result?;
        self.collations.validate_query(plan)?;
        Ok(output)
    }

    fn bind_recursive_seed(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match &plan.root {
            RelationalPlan::SetOp { left, .. } => self.bind_query(routines, left, params, outer),
            _ => self.bind_root(routines, &plan.root, params, outer),
        }
    }

    fn bind_root(
        &mut self,
        routines: &dyn RoutineResolution,
        root: &RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                self.bind_query_block(routines, block, params, outer)
            }
            RelationalPlan::SetOp { .. } => {
                preparation::set_metadata(self, routines, root, outer, params)
            }
            RelationalPlan::Values { rows, subqueries } => {
                self.bind_values_schema(routines, rows, subqueries, outer, params)
            }
        }
    }

    fn bind_query_block(
        &mut self,
        routines: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let captured_outer = outer
            .map(|schema| crate::semantics::view_rewrite::bind_outer_rewrite_scope(block, schema));
        let outer = captured_outer.as_deref();
        let source_rewrites = crate::semantics::view_rewrite::resolve_outer_sources(
            block,
            outer,
            &mut |source, subqueries, scope| {
                self.bind_source_input(routines, source, subqueries, params, scope)
            },
        )?;
        let block = source_rewrites.as_ref();
        let source = block.from.as_ref().map_or_else(
            || Ok(RowSchema::default()),
            |source| self.bind_source(routines, source, &block.subqueries, params, outer),
        )?;
        if let Some(sources) = &mut self.execution_sources {
            sources.record(block, &source, outer);
        }
        // Query-owned metadata types belong to both analysis and metadata-only binding.
        let source = analysis::with_query_source_columns(&source, block);
        let rewritten = crate::semantics::view_rewrite::resolve_outer_rewrites(block, &source);
        let block = rewritten.as_ref();
        let expression_schema = overlay_outer_schema(&source, outer);
        let output = preparation::block_metadata(self, routines, block, &source, outer, params)?;
        let output = analysis::with_projected_open_columns(&output, &block.projections, &source);
        if self.validate_references {
            self.validate_query_block_clauses(
                routines,
                block,
                &expression_schema,
                &output,
                params,
                outer,
            )?;
        }
        Ok(output)
    }

    fn bind_expression_type(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        if let ScalarExpr::ScalarSubquery(index) = expression {
            let plan = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
            })?;
            // Result typing and validation share the complete lexical row scope.
            let subquery_outer = Some(schema);
            let output = self.bind_query(routines, plan, params, subquery_outer)?;
            return Ok(output.column_type(0).cloned());
        }
        let schema = self.with_stored_outer_internal_aliases(schema);
        let schema = &schema;
        let resolver =
            self.query_function_type_resolver(routines, expression, schema, subqueries, params)?;
        if self.validate_references {
            Self::validate_expression_references_with_resolver(
                self.rule_condition_event,
                routines,
                expression,
                schema,
                None,
                params,
                &resolver,
            )?;
        }
        crate::type_resolution::validate_catalog_literals(expression, schema, params, &resolver)?;
        crate::scalar_type_with_resolver(expression, schema, params, &resolver)
    }

    fn bind_projection_metadata(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<projection::ProjectionMetadata, SQLError> {
        let schema = self.with_stored_outer_internal_aliases(schema);
        preparation::projection_metadata(self, routines, expression, &schema, subqueries, params)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn bind_source_inner(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<RowSchema, SQLError> {
        #[cfg(test)]
        SOURCE_BINDINGS.with(|count| count.set(count.get() + 1));
        let input = (!matches!(source, SourcePlan::Join { .. }))
            .then(|| scope.input(source))
            .flatten();
        let outer = input.as_ref();
        match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                let cte_name = crate::semantics::cte_reference_name(name);
                if let Some(schema) = cte_name.as_ref().and_then(|name| self.ctes.get(name)) {
                    if let Some(name) = cte_name
                        .as_ref()
                        .filter(|name| self.non_returning_ctes.contains(*name))
                    {
                        return Err(non_returning_cte_error(name));
                    }
                    return alias_table_schema(schema, qualifier, column_aliases);
                }
                if let Some(plan) = cte_name
                    .as_ref()
                    .and_then(|name| self.deferred_ctes.get(name))
                {
                    if !plan.body.returns_rows() {
                        return Err(non_returning_cte_error(&plan.name));
                    }
                }
                if let Some(plan) = cte_name.and_then(|name| self.deferred_ctes.remove(&name)) {
                    let result = self
                        .bind_cte_schemas(
                            routines,
                            std::slice::from_ref(plan.as_ref()),
                            params,
                            outer,
                        )
                        .and_then(|previous| {
                            let schema = self.ctes.get(&plan.name).expect("CTE schema was bound");
                            let result = alias_table_schema(schema, qualifier, column_aliases);
                            self.restore_cte_schemas(previous);
                            result
                        });
                    self.deferred_ctes.insert(plan.name.clone(), plan);
                    return result;
                }
                if self.catalog.sequence_exists(&self.resolution, name)? {
                    self.record_relation_dependency(name)?;
                    let schema = RowSchema::with_qualified_types(
                        qualifier,
                        vec!["last_value".into(), "log_cnt".into(), "is_called".into()],
                        vec![
                            Some(ColumnType::BigInteger),
                            Some(ColumnType::BigInteger),
                            Some(ColumnType::Boolean),
                        ],
                    );
                    return self.alias_relation_schema(&schema, name, qualifier, column_aliases);
                }
                let view = self.catalog.view_resolved(&self.resolution, name)?;
                if let Some(view) = view {
                    self.record_relation_dependency(name)?;
                    if view.materialized {
                        let columns = view.output_columns.unwrap_or_default();
                        if columns.len() != view.materialized_column_types.len() {
                            return Err(SQLError::Internal(format!(
                                "materialized view `{name}` has {} columns but {} stored column types",
                                columns.len(),
                                view.materialized_column_types.len()
                            )));
                        }
                        let schema = crate::catalog::view::materialized_view_schema(
                            &columns,
                            &view.materialized_column_types,
                            view.materialized_column_collations.as_deref(),
                        )?;
                        let schema = RowSchema::with_relation_qualifier(&schema, qualifier);
                        return self.alias_relation_schema(
                            &schema,
                            name,
                            qualifier,
                            column_aliases,
                        );
                    }
                    let key = name.to_ascii_lowercase();
                    if !self.visiting_views.insert(key.clone()) {
                        return Err(SQLError::Internal(format!(
                            "view `{name}` has a recursive schema dependency"
                        )));
                    }
                    self.record_stored_query_dependencies(&view.query);
                    // A view's stored query is bound as it was created; its set-returning calls are not placed again.
                    let result = {
                        let _stored = SetPlacementScope::stored_definition();
                        self.bind_query(routines, &view.query, params, outer)
                    }
                    .and_then(|schema| {
                        let schema = rename_schema(
                            &schema,
                            view.output_columns.as_deref().unwrap_or(&[]),
                            Some(qualifier),
                        );
                        self.alias_relation_schema(&schema, name, qualifier, column_aliases)
                    });
                    self.visiting_views.remove(&key);
                    return result;
                }
                let table = self.catalog.table_resolved(&self.resolution, name)?;
                if let Some(table) = table {
                    self.record_relation_dependency(name)?;
                    let schema =
                        RowSchema::with_column_definitions(Some(qualifier), &table.columns);
                    let schema =
                        crate::semantics::bound_source_schema(&schema, bound_columns.as_deref())?;
                    let schema =
                        self.alias_relation_schema(&schema, name, qualifier, column_aliases)?;
                    let schema = if table.columns.is_empty() && !table.columns_declared {
                        RowSchema::with_open_columns(&schema, Some(qualifier))
                    } else {
                        schema
                    };
                    return Ok(analysis::with_table_pseudo_columns(&schema, qualifier));
                }
                let foreign_table = self
                    .catalog
                    .foreign_table_resolved(&self.resolution, name)?;
                if let Some(foreign_table) = foreign_table {
                    self.record_relation_dependency(name)?;
                    let schema =
                        RowSchema::with_column_definitions(Some(qualifier), &foreign_table.columns);
                    let schema =
                        crate::semantics::bound_source_schema(&schema, bound_columns.as_deref())?;
                    return self.alias_relation_schema(&schema, name, qualifier, column_aliases);
                }
                if let Some(schema) = self
                    .catalog
                    .virtual_relation_schema(&self.resolution, name)?
                {
                    self.record_relation_dependency(name)?;
                    let (columns, types): (Vec<_>, Vec<_>) = schema
                        .into_iter()
                        .map(|(column, ty)| (column, Some(ty)))
                        .unzip();
                    let schema = RowSchema::with_qualified_types(qualifier, columns, types);
                    return self.alias_relation_schema(&schema, name, qualifier, column_aliases);
                }
                if let Some(relation) = self.catalog.unopenable_relation(&self.resolution, name)? {
                    return Err(relation.error());
                }
                Err(SQLError::UnknownTable(name.clone()))
            }
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                ..
            } => {
                if internal_relation.is_some()
                    && internal_column_types.iter().any(Option::is_none)
                    && !rows.is_empty()
                {
                    let inferred =
                        self.bind_values_schema(routines, rows, subqueries, outer, params)?;
                    if let Some(schema) =
                        source.internal_values_schema_with_inference(Some(&inferred))?
                    {
                        return Ok(schema);
                    }
                }
                if let Some(schema) = source.internal_values_schema()? {
                    return Ok(schema);
                }
                let schema = self.bind_values_schema(routines, rows, subqueries, outer, params)?;
                Ok(rename_schema(&schema, column_aliases, alias.as_deref()))
            }
            SourcePlan::RetainedRows(source) => Ok(source.schema().clone()),
            SourcePlan::Function {
                name,
                binding,
                output_name,
                relations,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
                ..
            } => {
                let lower = crate::semantics::builtin_function_dispatch_name(name);
                let operator_join = crate::registry::is_operator_join_table_function(&lower);
                let operator_inputs = operator_join
                    .then(|| {
                        operator_join_relation_schemas(
                            &self.catalog,
                            &self.resolution,
                            relations.as_ref(),
                        )
                    })
                    .transpose()?;
                let input = outer.cloned().unwrap_or_default();
                let type_resolver = self.query_function_type_resolver_for_subqueries(
                    routines, args, &input, subqueries, params,
                )?;
                let user_function = if let Some((left, right)) = operator_inputs.as_ref() {
                    if self.validate_references {
                        let constant = RowSchema::default();
                        for (position, argument) in args.iter().enumerate() {
                            let schema = match position {
                                0 => left,
                                1 => right,
                                _ => &constant,
                            };
                            self.validate_expression_references(
                                routines, argument, schema, None, subqueries, params,
                            )?;
                        }
                    }
                    None
                } else if self.validate_references {
                    self.validate_table_function_source(
                        routines,
                        analysis::TableFunctionSourceValidation {
                            name,
                            binding: binding.as_ref(),
                            args,
                            subqueries,
                            input: &input,
                            params,
                        },
                    )?
                } else {
                    crate::semantics::resolve_user_table_function(
                        routines,
                        name,
                        binding.as_ref(),
                        args,
                        &input,
                        params,
                        &type_resolver,
                    )?
                };
                validate_table_function_column_definition(
                    routines,
                    crate::semantics::ColumnDefinitionRequest {
                        name,
                        binding: binding.as_ref(),
                        user_function: user_function
                            .as_ref()
                            .map(|resolved| resolved.function.as_ref()),
                        user_invocation: user_function
                            .as_ref()
                            .and_then(|resolved| resolved.binding.invocation.as_deref()),
                        args,
                        declared_types: column_types,
                        column_names: column_aliases,
                    },
                    &input,
                    params,
                    &type_resolver,
                )?;
                if let Some(function) = &user_function {
                    self.record_routine_dependency(&function.binding);
                }
                let registered_columns = user_function
                    .is_none()
                    .then(|| routines.registered_table_function_columns(name))
                    .flatten();
                let declared_host_columns = registered_columns.is_some();
                if registered_columns.as_ref().is_some_and(Vec::is_empty) {
                    return Err(SQLError::TypeMismatch(format!(
                        "table function `{name}` declares no columns"
                    )));
                }
                let catalog_columns = if !self.validate_references && user_function.is_none() {
                    user_function_output_columns(self.catalog.as_ref(), &self.resolution, name)?
                } else {
                    None
                };
                let columns = if let Some(resolved) = &user_function {
                    crate::semantics::user_table_function_columns(
                        routines,
                        &resolved.function,
                        resolved.binding.invocation.as_deref(),
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        *ordinality,
                    )?
                } else {
                    registered_columns.or(catalog_columns).map_or_else(
                        || {
                            table_function_empty_schema(
                                name,
                                output_name,
                                alias.as_deref(),
                                column_aliases,
                                args.len(),
                                *ordinality,
                            )
                        },
                        |columns| {
                            apply_table_function_aliases(columns, column_aliases, *ordinality)
                        },
                    )
                };
                validate_table_function_alias_count(
                    alias.as_deref().unwrap_or(output_name),
                    columns.len(),
                    column_aliases.len(),
                )?;
                let types = table_function_column_types(
                    routines,
                    TableFunctionTypeRequest {
                        name,
                        binding: binding.as_ref(),
                        args,
                        user_function: user_function
                            .as_ref()
                            .map(|resolved| resolved.function.as_ref()),
                        user_invocation: user_function
                            .as_ref()
                            .and_then(|resolved| resolved.binding.invocation.as_deref()),
                        declared_types: column_types,
                        columns: &columns,
                        ordinality: *ordinality,
                    },
                    &input,
                    params,
                    &type_resolver,
                );
                let qualifier = alias.as_deref().unwrap_or(output_name);
                let schema = RowSchema::with_qualified_types(qualifier, columns, types);
                Ok(
                    if user_function.is_none()
                        && !declared_host_columns
                        && column_types.is_empty()
                        && routines.has_registered_table_function(name)
                    {
                        RowSchema::with_open_columns(&schema, Some(qualifier))
                    } else {
                        schema
                    },
                )
            }
            SourcePlan::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => {
                let first = functions
                    .first()
                    .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
                let mut columns = Vec::new();
                let mut types = Vec::new();
                let mut open = false;
                for function in functions {
                    let member = table_function_member_source(function);
                    let schema = self.bind_source(routines, &member, subqueries, params, outer)?;
                    open |= schema.columns_are_open(None);
                    columns.extend(schema.iter().enumerate().map(|(position, column)| {
                        schema.public_name(position).unwrap_or(column).to_string()
                    }));
                    types.extend(schema.column_types().iter().map(|ty| {
                        if open {
                            None
                        } else {
                            ty.clone()
                        }
                    }));
                }
                if *ordinality {
                    columns.push("ordinality".into());
                    types.push((!open).then_some(ColumnType::BigInteger));
                }
                let qualifier = alias.as_deref().unwrap_or(&first.output_name);
                // A runtime member can shift every later output position,
                // including ordinality. Its complete descriptor validates
                // the aliases and supplies those types at execution.
                if !open {
                    validate_table_function_alias_count(
                        qualifier,
                        columns.len(),
                        column_aliases.len(),
                    )?;
                }
                for (column, alias) in columns.iter_mut().zip(column_aliases) {
                    column.clone_from(alias);
                }
                let schema = RowSchema::with_qualified_types(qualifier, columns, types);
                Ok(if open {
                    RowSchema::with_open_columns(&schema, Some(qualifier))
                } else {
                    schema
                })
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
                ..
            } => {
                let schema = self.bind_query(routines, body, params, outer)?;
                Ok(match source {
                    SourcePlan::Subquery {
                        captured_output: Some(relation),
                        ..
                    } => RowSchema::capture_output(&schema, *relation),
                    _ => match alias {
                        Some(alias) if !schema.columns_are_open(None) => {
                            alias_table_schema(&schema, alias, column_aliases)?
                        }
                        alias => rename_schema(&schema, column_aliases, alias.as_deref()),
                    },
                })
            }
            SourcePlan::Join {
                left,
                right,
                kind,
                on,
                using,
                natural,
                alias,
                column_aliases,
                lateral,
                ..
            } => {
                let left_schema = self.bind_source_in_scope(
                    routines,
                    left,
                    subqueries,
                    params,
                    scope.with_names(),
                )?;
                let preceding = scope.right_namespace(left, &left_schema, *kind);
                let right_schema = self.bind_source_in_scope(
                    routines,
                    right,
                    subqueries,
                    params,
                    SourceScope {
                        outer: scope.outer,
                        preceding: Some(&preceding),
                        inherited_lateral: *lateral,
                        retain_names: true,
                    },
                )?;
                let condition = scope.condition();
                self.bind_join_output_schema(JoinSchemaBinding {
                    routines,
                    kind: *kind,
                    on: on.as_ref(),
                    using: using.as_ref(),
                    natural: *natural,
                    alias: alias.as_deref(),
                    column_aliases,
                    left: &left_schema,
                    right: &right_schema,
                    subqueries,
                    params,
                    outer: condition.as_ref(),
                })
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fixture;

pub mod correlation;

pub mod stored_columns;
pub mod stored_relations;
pub mod stored_routines;
pub mod stored_types;
pub mod syntax_sites;

pub mod scoped_types;
pub mod snapshot;

pub mod view_dependencies;

pub mod portals;
