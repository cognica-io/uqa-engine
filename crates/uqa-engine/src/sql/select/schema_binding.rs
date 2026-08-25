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
mod cte_controls;
mod routine_binding;

pub(in crate::sql) use cte_controls::{
    analyze_recursive_control_step, extend_cte_generated_schema,
};
pub(in crate::sql) use routine_binding::bind_query_plan_routines_for_storage;

use cte_controls::{extend_recursive_cte_binding_schema, hide_recursive_generated_schema};

use super::{
    cte_references_own_name, expr_contains_subquery, is_score_provenance_column, ordered_plan_ctes,
    projection_columns, user_function_output_columns, CteScope, Engine, QueryBlockPlan, QueryPlan,
    RelationalPlan, SQLError, SQLParam, ScalarExpr, SourcePlan, Value,
};
use crate::sql::from_rows::{
    alias_join_schema, apply_table_function_aliases, join_using_output_schema, resolve_join_using,
    table_function_column_types, table_function_empty_schema, validate_table_function_alias_count,
    validate_table_function_column_definition, TableFunctionTypeRequest,
};
use crate::sql::virtual_relation_schema;
use std::collections::{BTreeMap, BTreeSet};
use uqa_execution::{FunctionTypeResolver, ResolvedFunctionOverload, RowSchema};
use uqa_sql::ast::{ColumnType, JoinKind, JoinUsing};

type ProjectionStarColumn = (String, Option<ColumnType>);

#[derive(Default)]
struct SchemaScope {
    ctes: BTreeMap<String, RowSchema>,
    deferred_ctes: BTreeMap<String, uqa_planner::CtePlan>,
    visiting_views: BTreeSet<String>,
    validate_references: bool,
}

struct QueryFunctionTypeResolver<'a> {
    engine: &'a Engine,
    scalar_subquery_types: Vec<Option<ColumnType>>,
}

struct JoinSchemaBinding<'a> {
    engine: &'a Engine,
    kind: JoinKind,
    on: Option<&'a ScalarExpr>,
    using: Option<&'a JoinUsing>,
    natural: bool,
    alias: Option<&'a str>,
    column_aliases: &'a [String],
    left: &'a RowSchema,
    right: &'a RowSchema,
    subqueries: &'a [QueryPlan],
    params: &'a [SQLParam],
    outer: Option<&'a RowSchema>,
}

fn table_function_member_source(function: &uqa_planner::TableFunctionPlan) -> SourcePlan {
    SourcePlan::Function {
        name: function.name.clone(),
        binding: function.binding.clone(),
        output_name: function.output_name.clone(),
        relation: function.relation.clone(),
        args: function.args.clone(),
        alias: None,
        column_aliases: function.column_aliases.clone(),
        ordinality: false,
        column_types: function.column_types.clone(),
    }
}

impl FunctionTypeResolver for QueryFunctionTypeResolver<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.engine.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.engine.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.engine.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[uqa_execution::BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: uqa_execution::SubqueryId,
        _outer_schema: &RowSchema,
        _params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        self.scalar_subquery_types
            .get(subquery)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {subquery} is out of bounds"))
            })
    }
}

impl SchemaScope {
    fn query_function_type_resolver<'a>(
        &mut self,
        engine: &'a Engine,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Option<QueryFunctionTypeResolver<'a>>, SQLError> {
        self.query_function_type_resolver_for_subqueries(
            engine,
            expr_contains_subquery(expression),
            schema,
            subqueries,
            params,
            outer,
        )
    }

    fn query_function_type_resolver_for_subqueries<'a>(
        &mut self,
        engine: &'a Engine,
        contains_subquery: bool,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Option<QueryFunctionTypeResolver<'a>>, SQLError> {
        if subqueries.is_empty() || !contains_subquery {
            return Ok(None);
        }
        let subquery_outer = self.validate_references.then_some(schema).or(outer);
        let scalar_subquery_types = subqueries
            .iter()
            .map(|plan| {
                self.bind_query(engine, plan, params, subquery_outer)
                    .map(|output| output.column_type(0).cloned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(QueryFunctionTypeResolver {
            engine,
            scalar_subquery_types,
        }))
    }
}

/// Bind an operator join's relation argument as the input schema for its retrieval expressions.
fn operator_join_relation_schema(
    engine: &Engine,
    relation: Option<&str>,
) -> Result<RowSchema, SQLError> {
    let relation = relation.ok_or_else(|| {
        SQLError::TypeMismatch("operator join relation must be a table identifier".into())
    })?;
    let resolved = engine
        .try_resolve_table_name(relation)
        .map_err(|error| {
            SQLError::Internal(format!(
                "resolve operator join relation `{relation}` schema: {error}"
            ))
        })?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let identity = crate::RelationIdentity::from_legacy_name(&resolved).map_err(|error| {
        SQLError::Internal(format!(
            "decode operator join relation `{resolved}` schema: {error}"
        ))
    })?;
    let table = engine.try_table(&resolved).map_err(|error| {
        SQLError::Internal(format!(
            "read operator join relation `{resolved}` schema: {error}"
        ))
    })?;
    let table = table.ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let definitions = table.columns.read();
    let columns = definitions
        .iter()
        .map(|column| column.name.clone())
        .collect();
    let types = definitions
        .iter()
        .map(|column| Some(column.ty.clone()))
        .collect();
    let schema = RowSchema::with_qualified_types(&identity.name, columns, types);
    Ok(analysis::with_table_pseudo_columns(&schema, &identity.name))
}

impl SchemaScope {
    fn from_execution_scope(ctes: &CteScope) -> Self {
        Self {
            ctes: ctes
                .rows
                .iter()
                .map(|(name, rows)| {
                    let schema = rows.row_schema();
                    let schema = ctes.recursive_control_width(name).map_or_else(
                        || schema.clone(),
                        |visible| hide_recursive_generated_schema(schema, visible),
                    );
                    (name.clone(), schema)
                })
                .collect(),
            deferred_ctes: ctes.deferred_ctes().clone(),
            visiting_views: BTreeSet::new(),
            validate_references: false,
        }
    }

    fn for_analysis(ctes: &CteScope) -> Self {
        let mut scope = Self::from_execution_scope(ctes);
        scope.validate_references = true;
        scope
    }

    fn bind_query(
        &mut self,
        engine: &Engine,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.bind_query_mode(engine, plan, params, outer, false)
    }

    fn bind_set_operand(
        &mut self,
        engine: &Engine,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.bind_query_mode(engine, plan, params, outer, true)
    }

    fn bind_query_mode(
        &mut self,
        engine: &Engine,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        let mut previous = Vec::with_capacity(plan.ctes.len());
        for cte in ordered_plan_ctes(plan)? {
            let self_recursive = cte_references_own_name(cte);
            let provisional = if self_recursive {
                self.bind_recursive_seed(engine, &cte.query, params, outer)?
            } else {
                self.bind_query(engine, &cte.query, params, outer)?
            };
            let provisional = rename_schema(&provisional, &cte.columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(engine, cte, provisional, params)?
            } else {
                extend_cte_generated_schema(engine, cte, provisional, params)?
            };
            previous.push((
                cte.name.clone(),
                self.ctes.insert(cte.name.clone(), provisional),
            ));

            if self_recursive {
                let complete = self.bind_query(engine, &cte.query, params, outer)?;
                let complete = rename_schema(&complete, &cte.columns, None);
                let complete = extend_cte_generated_schema(engine, cte, complete, params)?;
                self.ctes.insert(cte.name.clone(), complete);
            }
        }

        let result = self.bind_root(
            engine,
            &plan.root,
            params,
            outer,
            preserve_top_level_unknown,
        );
        for (name, schema) in previous.into_iter().rev() {
            match schema {
                Some(schema) => {
                    self.ctes.insert(name, schema);
                }
                None => {
                    self.ctes.remove(&name);
                }
            }
        }
        result
    }

    fn bind_recursive_seed(
        &mut self,
        engine: &Engine,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match &plan.root {
            RelationalPlan::SetOp { left, .. } => self.bind_query(engine, left, params, outer),
            _ => self.bind_root(engine, &plan.root, params, outer, false),
        }
    }

    fn bind_root(
        &mut self,
        engine: &Engine,
        root: &RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                self.bind_query_block(engine, block, params, outer, preserve_top_level_unknown)
            }
            RelationalPlan::SetOp {
                left,
                right,
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                let left = self.bind_set_operand(engine, left, params, outer)?;
                let right = self.bind_set_operand(engine, right, params, outer)?;
                if left.len() != right.len() {
                    return Err(SQLError::TypeMismatch(format!(
                        "set operation has {} columns on the left and {} on the right",
                        left.len(),
                        right.len()
                    )));
                }
                let types = left
                    .column_types()
                    .iter()
                    .zip(right.column_types())
                    .map(|(left, right)| merge_types(left.as_ref(), right.as_ref()))
                    .collect::<Result<Vec<_>, _>>()?;
                let output = RowSchema::with_types(left.columns().to_vec(), types);
                if self.validate_references {
                    self.validate_set_operation_clauses(
                        engine,
                        analysis::SetOperationClauses {
                            order_by,
                            limit: limit.as_deref(),
                            offset: offset.as_deref(),
                            subqueries,
                            output: &output,
                        },
                        params,
                    )?;
                }
                Ok(output)
            }
            RelationalPlan::Values { rows, subqueries } => {
                let columns = rows.first().map_or_else(Vec::new, |row| {
                    (1..=row.len())
                        .map(|index| format!("column{index}"))
                        .collect()
                });
                let types =
                    self.bind_values_types(engine, rows, subqueries, outer, params, outer)?;
                Ok(RowSchema::with_types(columns, types))
            }
        }
    }

    fn bind_query_block(
        &mut self,
        engine: &Engine,
        block: &QueryBlockPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        let source = block.from.as_ref().map_or_else(
            || Ok(RowSchema::default()),
            |source| self.bind_source(engine, source, &block.subqueries, params, outer),
        )?;
        let source = if self.validate_references {
            analysis::with_unqualified_table_pseudo_columns(&source)
        } else {
            source
        };
        let expression_schema = overlay_outer_schema(&source, outer);
        let labels = projection_columns(&block.projections);
        let mut columns = Vec::new();
        let mut types = Vec::new();
        for (position, projection) in block.projections.iter().enumerate() {
            if let Some(star_columns) = projection_star_columns(&projection.expr, &source)? {
                for (column, ty) in star_columns {
                    columns.push(column);
                    types.push(ty);
                }
                continue;
            }
            columns.push(labels[position].clone());
            types.push(
                if preserve_top_level_unknown
                    && matches!(
                        &projection.expr,
                        ScalarExpr::Literal(Value::Str(_) | Value::Null)
                    )
                {
                    None
                } else {
                    self.bind_expression_type(
                        engine,
                        &projection.expr,
                        &expression_schema,
                        &block.subqueries,
                        params,
                        outer,
                    )?
                },
            );
        }
        let output = RowSchema::with_types(columns, types);
        if self.validate_references {
            self.validate_query_block_clauses(engine, block, &expression_schema, &output, params)?;
        }
        Ok(output)
    }

    fn bind_expression_type(
        &mut self,
        engine: &Engine,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Option<ColumnType>, SQLError> {
        if let ScalarExpr::ScalarSubquery(index) = expression {
            let plan = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
            })?;
            let subquery_outer = self.validate_references.then_some(schema).or(outer);
            let output = self.bind_query(engine, plan, params, subquery_outer)?;
            return Ok(output.column_type(0).cloned());
        }
        let resolver = self
            .query_function_type_resolver(engine, expression, schema, subqueries, params, outer)?;
        if self.validate_references {
            Self::validate_expression_references_with_resolver(
                engine,
                expression,
                schema,
                None,
                params,
                resolver
                    .as_ref()
                    .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver),
            )?;
        }
        uqa_execution::scalar_type_with_resolver(
            expression,
            schema,
            params,
            resolver
                .as_ref()
                .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver),
        )
    }

    fn bind_values_types(
        &mut self,
        engine: &Engine,
        rows: &[Vec<ScalarExpr>],
        subqueries: &[QueryPlan],
        schema: Option<&RowSchema>,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        let width = rows.first().map_or(0, Vec::len);
        let empty = RowSchema::default();
        let schema = schema.unwrap_or(&empty);
        let mut types = vec![None; width];
        for row in rows {
            if row.len() != width {
                return Err(SQLError::TypeMismatch(
                    "VALUES lists must all be the same length".into(),
                ));
            }
            for (position, expression) in row.iter().enumerate() {
                let candidate =
                    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                        None
                    } else {
                        self.bind_expression_type(
                            engine, expression, schema, subqueries, params, outer,
                        )?
                    };
                types[position] = merge_types(types[position].as_ref(), candidate.as_ref())?;
            }
        }
        Ok(types
            .into_iter()
            .map(|ty| ty.or(Some(ColumnType::Text)))
            .collect())
    }

    fn bind_join_output_schema(
        &mut self,
        binding: JoinSchemaBinding<'_>,
    ) -> Result<RowSchema, SQLError> {
        let JoinSchemaBinding {
            engine,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            left,
            right,
            subqueries,
            params,
            outer,
        } = binding;
        if let Some(on) = on {
            let input = RowSchema::join(left, right, std::iter::empty::<String>());
            let input = overlay_outer_schema(&input, outer);
            if self.validate_references {
                self.bind_expression_type(engine, on, &input, subqueries, params, outer)?;
            } else {
                uqa_execution::scalar_type_with_resolver(on, &input, params, engine)?;
            }
        }
        let resolved = resolve_join_using(using, natural, left, right)?;
        let schema = resolved.map_or_else(
            || Ok(RowSchema::join(left, right, std::iter::empty())),
            |using| join_using_output_schema(kind, left, right, &using),
        )?;
        alias_join_schema(&schema, alias, column_aliases)
    }

    fn bind_source(
        &mut self,
        engine: &Engine,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                if let Some(schema) = self.ctes.get(name) {
                    return Ok(rename_schema(schema, &[], Some(qualifier)));
                }
                if let Some(plan) = self.deferred_ctes.remove(name) {
                    let result = self
                        .bind_query(engine, &plan.query, params, outer)
                        .map(|schema| rename_schema(&schema, &plan.columns, Some(qualifier)));
                    self.deferred_ctes.insert(name.clone(), plan);
                    return result;
                }
                if let Some(view) = engine.view_definition(name)? {
                    if view.kind == crate::StoredViewKind::Materialized {
                        let columns = view.output_columns.unwrap_or_default();
                        if columns.len() != view.materialized_column_types.len() {
                            return Err(SQLError::Internal(format!(
                                "materialized view `{name}` has {} columns but {} stored column types",
                                columns.len(),
                                view.materialized_column_types.len()
                            )));
                        }
                        let schema = RowSchema::with_qualified_types(
                            qualifier,
                            columns,
                            view.materialized_column_types,
                        );
                        return Ok(schema);
                    }
                    let key = name.to_ascii_lowercase();
                    if !self.visiting_views.insert(key.clone()) {
                        return Err(SQLError::Internal(format!(
                            "view `{name}` has a recursive schema dependency"
                        )));
                    }
                    let result =
                        self.bind_query(engine, &view.query, params, outer)
                            .map(|schema| {
                                rename_schema(
                                    &schema,
                                    view.output_columns.as_deref().unwrap_or(&[]),
                                    Some(qualifier),
                                )
                            });
                    self.visiting_views.remove(&key);
                    return result;
                }
                if let Some(table) = engine.try_table(name).map_err(|error| {
                    SQLError::Internal(format!("resolve table `{name}` schema: {error}"))
                })? {
                    let definitions = table.columns.read();
                    let columns = definitions
                        .iter()
                        .map(|column| column.name.clone())
                        .collect();
                    let types = definitions
                        .iter()
                        .map(|column| Some(column.ty.clone()))
                        .collect();
                    let schema = RowSchema::with_qualified_types(qualifier, columns, types);
                    return Ok(if self.validate_references {
                        analysis::with_table_pseudo_columns(&schema, qualifier)
                    } else {
                        schema
                    });
                }
                if engine
                    .foreign_table(name)
                    .map_err(SQLError::Unsupported)?
                    .is_some()
                {
                    let typed_columns = engine
                        .foreign_table_typed_columns(name)
                        .map_err(SQLError::Unsupported)?;
                    let columns = typed_columns
                        .iter()
                        .map(|(column, _)| column.clone())
                        .collect();
                    let types = typed_columns.into_iter().map(|(_, ty)| Some(ty)).collect();
                    return Ok(RowSchema::with_qualified_types(qualifier, columns, types));
                }
                if let Some(schema) = virtual_relation_schema(engine, name)? {
                    let (columns, types): (Vec<_>, Vec<_>) = schema
                        .into_iter()
                        .map(|(column, ty)| (column, Some(ty)))
                        .unzip();
                    return Ok(RowSchema::with_qualified_types(qualifier, columns, types));
                }
                Err(SQLError::UnknownTable(name.clone()))
            }
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
            } => {
                let columns = if column_aliases.is_empty() {
                    (1..=rows.first().map_or(0, Vec::len))
                        .map(|index| format!("column{index}"))
                        .collect::<Vec<_>>()
                } else {
                    column_aliases.clone()
                };
                let binding_schema = outer.cloned().unwrap_or_default();
                let types = self.bind_values_types(
                    engine,
                    rows,
                    subqueries,
                    Some(&binding_schema),
                    params,
                    Some(&binding_schema),
                )?;
                Ok(match alias.as_deref() {
                    Some(qualifier) => RowSchema::with_qualified_types(qualifier, columns, types),
                    None => RowSchema::with_types(columns, types),
                })
            }
            SourcePlan::Function {
                name,
                binding,
                output_name,
                relation,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
                ..
            } => {
                let lower = crate::sql::builtin_function_dispatch_name(name);
                let input = if crate::operator_tree_bridge::is_operator_join_table_function(&lower)
                {
                    operator_join_relation_schema(engine, relation.as_deref())?
                } else {
                    outer.cloned().unwrap_or_default()
                };
                let type_resolver = self.query_function_type_resolver_for_subqueries(
                    engine,
                    args.iter().any(expr_contains_subquery),
                    &input,
                    subqueries,
                    params,
                    Some(&input),
                )?;
                let user_function = if self.validate_references {
                    self.validate_table_function_source(
                        engine,
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
                    crate::sql::from_rows::resolve_user_table_function(
                        engine,
                        name,
                        binding.as_ref(),
                        args,
                        &input,
                        params,
                        type_resolver
                            .as_ref()
                            .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver),
                    )?
                };
                validate_table_function_column_definition(
                    name,
                    binding.as_ref(),
                    user_function
                        .as_ref()
                        .map(|resolved| resolved.function.as_ref()),
                    column_types,
                )?;
                let columns = user_function
                    .as_ref()
                    .and_then(|resolved| {
                        crate::sql::from_rows::user_function_output_columns_for(&resolved.function)
                    })
                    .or_else(|| {
                        (!self.validate_references && user_function.is_none())
                            .then(|| user_function_output_columns(engine, name))
                            .flatten()
                    })
                    .map_or_else(
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
                    );
                validate_table_function_alias_count(
                    alias.as_deref().unwrap_or(output_name),
                    columns.len(),
                    column_aliases.len(),
                )?;
                let types = table_function_column_types(
                    engine,
                    TableFunctionTypeRequest {
                        name,
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
                    type_resolver
                        .as_ref()
                        .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver),
                );
                let qualifier = alias.as_deref().unwrap_or(output_name);
                Ok(RowSchema::with_qualified_types(qualifier, columns, types))
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
                for function in functions {
                    let member = table_function_member_source(function);
                    let schema = self.bind_source(engine, &member, subqueries, params, outer)?;
                    columns.extend(schema.iter().enumerate().map(|(position, column)| {
                        schema.public_name(position).unwrap_or(column).to_string()
                    }));
                    types.extend(schema.column_types().iter().cloned());
                }
                if *ordinality {
                    columns.push("ordinality".into());
                    types.push(Some(ColumnType::BigInteger));
                }
                let qualifier = alias.as_deref().unwrap_or(&first.output_name);
                validate_table_function_alias_count(
                    qualifier,
                    columns.len(),
                    column_aliases.len(),
                )?;
                for (column, alias) in columns.iter_mut().zip(column_aliases) {
                    column.clone_from(alias);
                }
                Ok(RowSchema::with_qualified_types(qualifier, columns, types))
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let schema = self.bind_query(engine, body, params, outer)?;
                Ok(rename_schema(&schema, column_aliases, alias.as_deref()))
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
                let left_schema = self.bind_source(engine, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_schema = self.bind_source(
                    engine,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
                self.bind_join_output_schema(JoinSchemaBinding {
                    engine,
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
                    outer,
                })
            }
        }
    }

    fn bind_source_for_execution(
        &mut self,
        engine: &Engine,
        source: &mut SourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match source {
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
                let left_schema =
                    self.bind_source_for_execution(engine, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_schema = self.bind_source_for_execution(
                    engine,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
                return self.bind_join_output_schema(JoinSchemaBinding {
                    engine,
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
                    outer,
                });
            }
            SourcePlan::Function {
                name,
                binding,
                relation,
                args,
                ..
            } => {
                let lower = crate::sql::builtin_function_dispatch_name(name);
                let input = if crate::operator_tree_bridge::is_operator_join_table_function(&lower)
                {
                    operator_join_relation_schema(engine, relation.as_deref())?
                } else {
                    outer.cloned().unwrap_or_default()
                };
                let resolver = self.query_function_type_resolver_for_subqueries(
                    engine,
                    args.iter().any(expr_contains_subquery),
                    &input,
                    subqueries,
                    params,
                    Some(&input),
                )?;
                let resolver = resolver
                    .as_ref()
                    .map_or(engine as &dyn FunctionTypeResolver, |resolver| resolver);
                if let Some(selected) = crate::sql::from_rows::resolve_table_function_binding(
                    engine,
                    name,
                    binding.as_ref(),
                    args,
                    &input,
                    params,
                    resolver,
                )? {
                    *binding = Some(selected);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    let mut member = table_function_member_source(function);
                    self.bind_source_for_execution(engine, &mut member, subqueries, params, outer)?;
                    let SourcePlan::Function { binding, .. } = member else {
                        unreachable!("table-function member changed source kind during binding")
                    };
                    function.binding = binding;
                }
            }
            SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Subquery { .. } => {}
        }
        self.bind_source(engine, source, subqueries, params, outer)
    }
}

/// Derive the exact output row type of a query plan without executing it.
pub(in crate::sql) fn bind_query_plan_schema(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes).bind_query(engine, plan, params, outer)
}

/// Derive the declared SQL type of a standalone expression plan without executing it. The plan-owned subquery arena participates in type resolution so scalar subqueries retain their projected type at command boundaries such as `CALL`.
pub(in crate::sql) fn bind_expression_plan_type(
    engine: &Engine,
    plan: &uqa_planner::ExpressionPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::from_execution_scope(ctes).bind_expression_type(
        engine,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
        None,
    )
}

/// Analyze every catalog and scalar reference and derive the exact output row type without executing the query.
pub(in crate::sql) fn analyze_query_plan_schema(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes).bind_query(engine, plan, params, outer)
}

/// Derive the exact row type of one FROM source without executing it.
pub(in crate::sql) fn bind_source_plan_schema(
    engine: &Engine,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes).bind_source(
        engine,
        source,
        &ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Bind every table-function source in one execution-owned source plan to its exact routine identity and return the schema derived from those same bindings.
pub(in crate::sql) fn bind_source_plan_schema_for_execution(
    engine: &Engine,
    source: &mut SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes).bind_source_for_execution(
        engine,
        source,
        &ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Bind a projection against an already-declared input schema. `star_schema`
/// identifies the relation expanded by bare `*`; `expression_schema` may also
/// contain joined sources and hidden lookup aliases used by scalar expressions.
pub(in crate::sql) fn bind_projection_output_schema(
    engine: &Engine,
    projections: &[uqa_planner::ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<RowSchema, SQLError> {
    let mut scope = SchemaScope::from_execution_scope(ctes);
    let labels = projection_columns(projections);
    let mut columns = Vec::new();
    let mut types = Vec::new();
    for (position, projection) in projections.iter().enumerate() {
        let expansion_schema = match projection.expr {
            ScalarExpr::QualifiedStar(_) => expression_schema,
            _ => star_schema,
        };
        if let Some(star_columns) = projection_star_columns(&projection.expr, expansion_schema)? {
            for (column, ty) in star_columns {
                columns.push(column);
                types.push(ty);
            }
            continue;
        }
        columns.push(labels[position].clone());
        types.push(scope.bind_expression_type(
            engine,
            &projection.expr,
            expression_schema,
            subqueries,
            params,
            Some(expression_schema),
        )?);
    }
    Ok(RowSchema::with_types(columns, types))
}

/// Validate every scalar expression in a query block while the physical input
/// still carries declared SQL types. This phase must precede polymorphic
/// rewrites such as `pg_typeof`, because an invalid common type is an error,
/// not an `unknown` result.
pub(in crate::sql) fn validate_query_block_expression_types(
    engine: &Engine,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let scalar_subquery_types = statement
        .subqueries
        .iter()
        .map(|plan| {
            bind_query_plan_schema(engine, plan, params, ctes, Some(schema))
                .map(|output| output.column_type(0).cloned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let resolver = QueryFunctionTypeResolver {
        engine,
        scalar_subquery_types,
    };
    for expression in statement
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(statement.group_by.iter())
        .chain(statement.grouping_sets.iter().flatten())
        .chain(statement.order_by.iter().map(|order| &order.expr))
        .chain(statement.distinct_on.iter())
        .chain(statement.r#where.iter())
        .chain(statement.having.iter())
        .chain(statement.limit.iter())
        .chain(statement.offset.iter())
    {
        uqa_execution::scalar_type_with_resolver(expression, schema, params, &resolver)?;
    }
    Ok(())
}

fn projection_star_columns(
    expression: &ScalarExpr,
    schema: &RowSchema,
) -> Result<Option<Vec<ProjectionStarColumn>>, SQLError> {
    match expression {
        ScalarExpr::Star => Ok(Some(
            schema
                .columns()
                .iter()
                .enumerate()
                .filter(|(_, column)| !is_score_provenance_column(column))
                .map(|(position, column)| {
                    (
                        schema.public_name(position).unwrap_or(column).to_string(),
                        schema.column_type(position).cloned(),
                    )
                })
                .collect(),
        )),
        ScalarExpr::QualifiedStar(qualifier) => {
            let columns = schema
                .qualified_star_layout(qualifier)
                .into_iter()
                .filter(|(column, _, _)| !is_score_provenance_column(column))
                .map(|(column, _, ty)| (column, ty))
                .collect::<Vec<_>>();
            if columns.is_empty() {
                return Err(SQLError::UnknownTable(qualifier.clone()));
            }
            Ok(Some(columns))
        }
        _ => Ok(None),
    }
}

fn rename_schema(schema: &RowSchema, aliases: &[String], qualifier: Option<&str>) -> RowSchema {
    let columns: Vec<String> = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            let base = if is_score_provenance_column(column) {
                column.clone()
            } else {
                aliases
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
            };
            base
        })
        .collect();
    let renamed = match qualifier {
        Some(qualifier) => {
            RowSchema::with_qualified_types(qualifier, columns, schema.column_types().to_vec())
        }
        None => RowSchema::with_types(columns, schema.column_types().to_vec()),
    };
    let mut hidden = Vec::new();
    let mut conflicting = Vec::new();
    for (identity, ty) in schema.typed_virtual_identities() {
        let conflicts = match identity.qualifier() {
            Some(source) => schema.qualified_column_is_ambiguous(source, identity.column()),
            None => schema.column_is_ambiguous(identity.column()),
        };
        let mapped = qualifier.map_or_else(
            || vec![identity.clone()],
            |qualifier| {
                vec![
                    uqa_execution::ColumnIdentity::unqualified(identity.column()),
                    uqa_execution::ColumnIdentity::qualified(qualifier, identity.column()),
                ]
            },
        );
        for identity in mapped {
            if conflicts {
                conflicting.push((identity, ty.cloned()));
            } else {
                hidden.push((identity, ty.cloned()));
            }
        }
    }
    let renamed = RowSchema::with_typed_virtual_identities(&renamed, &hidden);
    RowSchema::with_typed_conflicting_virtual_identities(&renamed, &conflicting)
}

pub(in crate::sql) fn overlay_outer_schema(
    current: &RowSchema,
    outer: Option<&RowSchema>,
) -> RowSchema {
    let Some(outer) = outer else {
        return current.clone();
    };
    let columns = outer
        .identities()
        .iter()
        .enumerate()
        .map(|(position, identity)| (identity.clone(), outer.column_type(position).cloned()))
        .collect::<Vec<_>>();
    let schema = RowSchema::with_typed_outer_identities(current, &columns);
    let virtual_identities = outer
        .typed_virtual_identities()
        .map(|(identity, ty)| (identity.clone(), ty.cloned()))
        .collect::<Vec<_>>();
    RowSchema::with_typed_virtual_identities(&schema, &virtual_identities)
}

pub(in crate::sql) fn values_types_in_scope(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    SchemaScope::from_execution_scope(ctes)
        .bind_values_types(engine, rows, subqueries, schema, params, schema)
}

fn merge_types(
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, None) => Ok(None),
        (Some(ty), None) | (None, Some(ty)) => Ok(Some(ty.clone())),
        (Some(left), Some(right)) => uqa_execution::common_type(left, right).map(Some),
    }
}
