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

use super::{
    cte_references_own_name, is_score_provenance_column, ordered_plan_ctes, projection_columns,
    user_function_output_columns, CteScope, Engine, QueryBlockPlan, QueryPlan, RelationalPlan,
    SQLError, SQLParam, ScalarExpr, SourcePlan, Value,
};
use crate::sql::from_rows::{
    join_using_output_schema, resolve_join_using, table_function_column_types,
    table_function_empty_schema,
};
use crate::sql::virtual_relation_schema;
use std::collections::{BTreeMap, BTreeSet};
use uqa_execution::RowSchema;
use uqa_sql::ast::ColumnType;

type ProjectionStarColumn = (String, Option<ColumnType>);

#[derive(Default)]
struct SchemaScope {
    ctes: BTreeMap<String, RowSchema>,
    deferred_ctes: BTreeMap<String, uqa_planner::CtePlan>,
    visiting_views: BTreeSet<String>,
}

impl SchemaScope {
    fn from_execution_scope(ctes: &CteScope) -> Self {
        Self {
            ctes: ctes
                .rows
                .iter()
                .map(|(name, rows)| (name.clone(), rows.row_schema().clone()))
                .collect(),
            deferred_ctes: ctes.deferred_ctes().clone(),
            visiting_views: BTreeSet::new(),
        }
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
            previous.push((
                cte.name.clone(),
                self.ctes.insert(cte.name.clone(), provisional),
            ));

            if self_recursive {
                let complete = self.bind_query(engine, &cte.query, params, outer)?;
                self.ctes.insert(
                    cte.name.clone(),
                    rename_schema(&complete, &cte.columns, None),
                );
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
            RelationalPlan::SetOp { left, right, .. } => {
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
                Ok(RowSchema::with_types(left.columns().to_vec(), types))
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
        Ok(RowSchema::with_types(columns, types))
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
            let output = self.bind_query(engine, plan, params, outer)?;
            return Ok(output.column_type(0).cloned());
        }
        uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)
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
                if let Some(plan) = engine.view_plan(name)? {
                    let key = name.to_ascii_lowercase();
                    if !self.visiting_views.insert(key.clone()) {
                        return Err(SQLError::Internal(format!(
                            "view `{name}` has a recursive schema dependency"
                        )));
                    }
                    let result = self
                        .bind_query(engine, &plan, params, outer)
                        .map(|schema| rename_schema(&schema, &[], Some(qualifier)));
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
                    return Ok(RowSchema::with_qualified_types(qualifier, columns, types));
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
                if let Some(schema) = virtual_relation_schema(engine, name) {
                    let (columns, types): (Vec<_>, Vec<_>) = schema
                        .into_iter()
                        .map(|(column, ty)| (column, Some(ty)))
                        .unzip();
                    return Ok(RowSchema::with_qualified_types(qualifier, columns, types));
                }
                Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )))
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
                output_name,
                args,
                alias,
                column_aliases,
                column_types,
                ..
            } => {
                let columns = if column_aliases.is_empty() {
                    user_function_output_columns(engine, name).map_or_else(
                        || {
                            table_function_empty_schema(
                                name,
                                output_name,
                                alias.as_deref(),
                                column_aliases,
                                args.len(),
                            )
                        },
                        |columns| columns,
                    )
                } else {
                    table_function_empty_schema(
                        name,
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        args.len(),
                    )
                };
                let input = outer.cloned().unwrap_or_default();
                let types = table_function_column_types(
                    engine,
                    name,
                    args,
                    column_types,
                    &columns,
                    &input,
                    params,
                );
                let qualifier = alias.as_deref().unwrap_or(output_name);
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
                using,
                natural,
                lateral,
                ..
            } => {
                let left_schema = self.bind_source(engine, left, subqueries, params, outer)?;
                let implicit_lateral_function =
                    matches!(right.as_ref(), SourcePlan::Function { .. });
                let right_scope = (*lateral || implicit_lateral_function)
                    .then(|| overlay_outer_schema(&left_schema, outer));
                let right_schema = self.bind_source(
                    engine,
                    right,
                    subqueries,
                    params,
                    right_scope.as_ref().or(outer),
                )?;
                let resolved =
                    resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
                resolved.map_or_else(
                    || {
                        Ok(RowSchema::join(
                            &left_schema,
                            &right_schema,
                            std::iter::empty(),
                        ))
                    },
                    |using| join_using_output_schema(*kind, &left_schema, &right_schema, &using),
                )
            }
        }
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
) -> Result<(), SQLError> {
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
        uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)?;
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
    match qualifier {
        Some(qualifier) => {
            RowSchema::with_qualified_types(qualifier, columns, schema.column_types().to_vec())
        }
        None => RowSchema::with_types(columns, schema.column_types().to_vec()),
    }
}

fn overlay_outer_schema(current: &RowSchema, outer: Option<&RowSchema>) -> RowSchema {
    let Some(outer) = outer else {
        return current.clone();
    };
    let columns = outer
        .identities()
        .iter()
        .enumerate()
        .map(|(position, identity)| (identity.clone(), outer.column_type(position).cloned()))
        .collect::<Vec<_>>();
    RowSchema::with_typed_outer_identities(current, &columns)
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
