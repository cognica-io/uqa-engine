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
mod catalog_sources;
mod cte_controls;
mod projection;
mod routine_binding;
mod scope;
mod sources;
mod type_resolution;

#[cfg(test)]
mod tests;

pub(in crate::sql) use cte_controls::{
    analyze_recursive_control_step, extend_cte_generated_schema,
};
pub(in crate::sql) use projection::{
    analyze_projection_output_schema, bind_projection_output_schema,
    validate_query_block_expression_types, validate_query_block_projection_references,
};
pub(in crate::sql) use routine_binding::bind_query_plan_routines_for_storage;
pub(in crate::sql) use scope::{
    analyze_query_plan_schema, analyze_query_plan_schema_with_catalog, bind_expression_plan_type,
    bind_query_plan_schema,
};
pub(in crate::sql) use scope::{overlay_outer_schema, values_types_in_scope};
pub(in crate::sql) use sources::{
    analyze_source_plan_schema, bind_source_plan_schema, bind_source_plan_schema_for_execution,
    with_query_table_pseudo_columns,
};

use catalog_sources::operator_join_relation_schemas;
use cte_controls::{extend_recursive_cte_binding_schema, hide_recursive_generated_schema};
use projection::{projection_star_columns, rename_schema};
use scope::merge_types;
use sources::{table_function_member_source, JoinSchemaBinding};
use type_resolution::QueryFunctionTypeResolver;

use super::{
    cte_references_own_name, expr_contains_subquery, ordered_plan_ctes, projection_columns,
    user_function_output_columns, CteScope, QueryBlockPlan, QueryPlan, RelationalPlan, SQLError,
    SQLParam, ScalarExpr, SourcePlan, Value,
};
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::engine_user_functions::RoutineResolution;
use crate::sql::from_rows::{
    alias_join_schema, apply_table_function_aliases, join_using_output_schema, resolve_join_using,
    table_function_column_types, table_function_empty_schema, validate_table_function_alias_count,
    validate_table_function_column_definition, TableFunctionTypeRequest,
};
use crate::sql::virtual_relation_schema;
use std::collections::{BTreeMap, BTreeSet};
use uqa_execution::RowSchema;
use uqa_sql::ast::ColumnType;

struct SchemaScope {
    catalog: CatalogReadView,
    resolution: RelationNameResolution,
    ctes: BTreeMap<String, RowSchema>,
    deferred_ctes: BTreeMap<String, uqa_planner::CtePlan>,
    visiting_views: BTreeSet<String>,
    validate_references: bool,
}

impl SchemaScope {
    fn query_function_type_resolver<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        self.query_function_type_resolver_for_subqueries(
            routines,
            expr_contains_subquery(expression),
            schema,
            subqueries,
            params,
            outer,
        )
    }

    fn query_function_type_resolver_for_subqueries<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        contains_subquery: bool,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        if subqueries.is_empty() || !contains_subquery {
            return Ok(QueryFunctionTypeResolver {
                routines,
                scalar_subquery_types: None,
            });
        }
        let subquery_outer = self.validate_references.then_some(schema).or(outer);
        let scalar_subquery_types = subqueries
            .iter()
            .map(|plan| {
                self.bind_query(routines, plan, params, subquery_outer)
                    .map(|output| output.column_type(0).cloned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueryFunctionTypeResolver {
            routines,
            scalar_subquery_types: Some(scalar_subquery_types),
        })
    }
}

impl SchemaScope {
    fn from_execution_scope(ctes: &CteScope) -> Result<Self, SQLError> {
        Ok(Self {
            catalog: ctes.catalog_read_view()?,
            resolution: ctes.relation_name_resolution()?,
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
        })
    }

    fn for_analysis(ctes: &CteScope) -> Result<Self, SQLError> {
        let mut scope = Self::from_execution_scope(ctes)?;
        scope.validate_references = true;
        Ok(scope)
    }

    fn for_catalog_analysis(catalog: CatalogReadView, resolution: RelationNameResolution) -> Self {
        Self {
            catalog,
            resolution,
            ctes: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            visiting_views: BTreeSet::new(),
            validate_references: true,
        }
    }

    fn bind_query(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.bind_query_mode(routines, plan, params, outer, false)
    }

    fn bind_set_operand(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.bind_query_mode(routines, plan, params, outer, true)
    }

    fn bind_query_mode(
        &mut self,
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        let mut previous = Vec::with_capacity(plan.ctes.len());
        for cte in ordered_plan_ctes(plan)? {
            let self_recursive = cte_references_own_name(cte);
            let provisional = if self_recursive {
                self.bind_recursive_seed(routines, &cte.query, params, outer)?
            } else {
                self.bind_query(routines, &cte.query, params, outer)?
            };
            let provisional = rename_schema(&provisional, &cte.columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(routines, cte, provisional, params)?
            } else {
                extend_cte_generated_schema(routines, cte, provisional, params)?
            };
            previous.push((
                cte.name.clone(),
                self.ctes.insert(cte.name.clone(), provisional),
            ));

            if self_recursive {
                let complete = self.bind_query(routines, &cte.query, params, outer)?;
                let complete = rename_schema(&complete, &cte.columns, None);
                let complete = extend_cte_generated_schema(routines, cte, complete, params)?;
                self.ctes.insert(cte.name.clone(), complete);
            }
        }

        let result = self.bind_root(
            routines,
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
        routines: &dyn RoutineResolution,
        plan: &QueryPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match &plan.root {
            RelationalPlan::SetOp { left, .. } => self.bind_query(routines, left, params, outer),
            _ => self.bind_root(routines, &plan.root, params, outer, false),
        }
    }

    fn bind_root(
        &mut self,
        routines: &dyn RoutineResolution,
        root: &RelationalPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => {
                self.bind_query_block(routines, block, params, outer, preserve_top_level_unknown)
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
                let left = self.bind_set_operand(routines, left, params, outer)?;
                let right = self.bind_set_operand(routines, right, params, outer)?;
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
                        routines,
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
                    self.bind_values_types(routines, rows, subqueries, outer, params, outer)?;
                Ok(RowSchema::with_types(columns, types))
            }
        }
    }

    fn bind_query_block(
        &mut self,
        routines: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        preserve_top_level_unknown: bool,
    ) -> Result<RowSchema, SQLError> {
        let source = block.from.as_ref().map_or_else(
            || Ok(RowSchema::default()),
            |source| self.bind_source(routines, source, &block.subqueries, params, outer),
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
                        routines,
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
            self.validate_query_block_clauses(
                routines,
                block,
                &expression_schema,
                &output,
                params,
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
        outer: Option<&RowSchema>,
    ) -> Result<Option<ColumnType>, SQLError> {
        if let ScalarExpr::ScalarSubquery(index) = expression {
            let plan = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
            })?;
            let subquery_outer = self.validate_references.then_some(schema).or(outer);
            let output = self.bind_query(routines, plan, params, subquery_outer)?;
            return Ok(output.column_type(0).cloned());
        }
        let resolver = self.query_function_type_resolver(
            routines, expression, schema, subqueries, params, outer,
        )?;
        if self.validate_references {
            Self::validate_expression_references_with_resolver(
                routines, expression, schema, None, params, &resolver,
            )?;
        }
        uqa_execution::scalar_type_with_resolver(expression, schema, params, &resolver)
    }

    fn bind_values_types(
        &mut self,
        routines: &dyn RoutineResolution,
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
                            routines, expression, schema, subqueries, params, outer,
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
            routines,
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
                self.bind_expression_type(routines, on, &input, subqueries, params, outer)?;
            } else {
                uqa_execution::scalar_type_with_resolver(on, &input, params, routines)?;
            }
        }
        let resolved = resolve_join_using(using, natural, left, right)?;
        let schema = resolved.map_or_else(
            || Ok(RowSchema::join(left, right, std::iter::empty())),
            |using| join_using_output_schema(kind, left, right, &using),
        )?;
        alias_join_schema(&schema, alias, column_aliases)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn bind_source(
        &mut self,
        routines: &dyn RoutineResolution,
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
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                if let Some(schema) = self.ctes.get(name) {
                    return Ok(rename_schema(schema, &[], Some(qualifier)));
                }
                if let Some(plan) = self.deferred_ctes.remove(name) {
                    let result = self
                        .bind_query(routines, &plan.query, params, outer)
                        .map(|schema| rename_schema(&schema, &plan.columns, Some(qualifier)));
                    self.deferred_ctes.insert(name.clone(), plan);
                    return result;
                }
                if self
                    .catalog
                    .sequence_resolved(&self.resolution, name)?
                    .is_some()
                {
                    return Ok(RowSchema::with_qualified_types(
                        qualifier,
                        vec!["last_value".into(), "log_cnt".into(), "is_called".into()],
                        vec![
                            Some(ColumnType::BigInteger),
                            Some(ColumnType::BigInteger),
                            Some(ColumnType::Boolean),
                        ],
                    ));
                }
                let view = self.catalog.view_resolved(&self.resolution, name)?.cloned();
                if let Some(view) = view {
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
                        self.bind_query(routines, &view.query, params, outer)
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
                let table = self.catalog.table_resolved(&self.resolution, name)?;
                if let Some(table) = table {
                    let columns = table
                        .columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect();
                    let types = table
                        .columns
                        .iter()
                        .map(|column| Some(column.ty.clone()))
                        .collect();
                    let schema = RowSchema::with_qualified_types(qualifier, columns, types);
                    return Ok(analysis::with_table_pseudo_columns(&schema, qualifier));
                }
                let foreign_table = self
                    .catalog
                    .foreign_table_resolved(&self.resolution, name)?;
                if let Some(foreign_table) = foreign_table {
                    let typed_columns = foreign_table
                        .columns
                        .iter()
                        .map(|column| {
                            (
                                column.name.clone(),
                                crate::engine_fdw::fdw_column_type_to_sql(&column.ty),
                            )
                        })
                        .collect::<Vec<_>>();
                    let columns = typed_columns
                        .iter()
                        .map(|(column, _)| column.clone())
                        .collect();
                    let types = typed_columns.into_iter().map(|(_, ty)| Some(ty)).collect();
                    return Ok(RowSchema::with_qualified_types(qualifier, columns, types));
                }
                if let Some(schema) =
                    virtual_relation_schema(&self.catalog, &self.resolution, name)?
                {
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
                internal_relation,
                internal_column_types,
            } => {
                if let Some(relation) = internal_relation {
                    return Ok(RowSchema::with_internal_relation_types(
                        *relation,
                        internal_column_types.clone(),
                    ));
                }
                let columns = if column_aliases.is_empty() {
                    (1..=rows.first().map_or(0, Vec::len))
                        .map(|index| format!("column{index}"))
                        .collect::<Vec<_>>()
                } else {
                    column_aliases.clone()
                };
                let binding_schema = outer.cloned().unwrap_or_default();
                let types = self.bind_values_types(
                    routines,
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
                relations,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
                ..
            } => {
                let lower = crate::sql::builtin_function_dispatch_name(name);
                let operator_join =
                    crate::operator_tree_bridge::is_operator_join_table_function(&lower);
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
                    routines,
                    args.iter().any(expr_contains_subquery),
                    &input,
                    subqueries,
                    params,
                    Some(&input),
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
                    crate::sql::from_rows::resolve_user_table_function(
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
                    name,
                    binding.as_ref(),
                    user_function
                        .as_ref()
                        .map(|resolved| resolved.function.as_ref()),
                    column_types,
                )?;
                let catalog_columns = if !self.validate_references && user_function.is_none() {
                    user_function_output_columns(&self.catalog, &self.resolution, name)?
                } else {
                    None
                };
                let columns = user_function
                    .as_ref()
                    .and_then(|resolved| {
                        crate::sql::from_rows::user_function_output_columns_for(&resolved.function)
                    })
                    .or(catalog_columns)
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
                    routines,
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
                    &type_resolver,
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
                    let schema = self.bind_source(routines, &member, subqueries, params, outer)?;
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
                let schema = self.bind_query(routines, body, params, outer)?;
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
                let left_schema = self.bind_source(routines, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_schema = self.bind_source(
                    routines,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
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
                    outer,
                })
            }
        }
    }

    fn bind_source_for_execution(
        &mut self,
        routines: &dyn RoutineResolution,
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
                    self.bind_source_for_execution(routines, left, subqueries, params, outer)?;
                let implicit_lateral_function = matches!(
                    right.as_ref(),
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_schema = self.bind_source_for_execution(
                    routines,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
                return self.bind_join_output_schema(JoinSchemaBinding {
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
                    outer,
                });
            }
            SourcePlan::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                let lower = crate::sql::builtin_function_dispatch_name(name);
                if crate::operator_tree_bridge::is_operator_join_table_function(&lower) {
                    operator_join_relation_schemas(
                        &self.catalog,
                        &self.resolution,
                        relations.as_ref(),
                    )?;
                    return self.bind_source(routines, source, subqueries, params, outer);
                }
                let input = outer.cloned().unwrap_or_default();
                let resolver = self.query_function_type_resolver_for_subqueries(
                    routines,
                    args.iter().any(expr_contains_subquery),
                    &input,
                    subqueries,
                    params,
                    Some(&input),
                )?;
                if let Some(selected) = crate::sql::from_rows::resolve_table_function_binding(
                    routines,
                    name,
                    binding.as_ref(),
                    args,
                    &input,
                    params,
                    &resolver,
                )? {
                    *binding = Some(selected);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    let mut member = table_function_member_source(function);
                    self.bind_source_for_execution(
                        routines,
                        &mut member,
                        subqueries,
                        params,
                        outer,
                    )?;
                    let SourcePlan::Function { binding, .. } = member else {
                        unreachable!("table-function member changed source kind during binding")
                    };
                    function.binding = binding;
                }
            }
            SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Subquery { .. } => {}
        }
        self.bind_source(routines, source, subqueries, params, outer)
    }
}
