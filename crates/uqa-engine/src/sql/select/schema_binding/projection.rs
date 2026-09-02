//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection row-type binding and reference validation.

use uqa_execution::RowSchema;
use uqa_planner::{ProjectionPlan, QueryBlockPlan, QueryPlan};
use uqa_sql::ast::ColumnType;
use uqa_sql::{SQLError, SQLParam};

use super::{
    bind_query_plan_schema, projection_columns, CteScope, QueryFunctionTypeResolver, ScalarExpr,
    SchemaScope,
};
use crate::engine_user_functions::RoutineResolution;

type ProjectionStarColumn = (String, Option<ColumnType>);

/// Bind a projection against an already-declared input schema. `star_schema` identifies the relation expanded by bare `*`; `expression_schema` may also contain joined sources and hidden lookup aliases used by scalar expressions.
pub(in crate::sql) fn bind_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<RowSchema, SQLError> {
    projection_output_schema(
        SchemaScope::from_execution_scope(ctes)?,
        routines,
        projections,
        expression_schema,
        star_schema,
        subqueries,
        params,
    )
}

/// Derive and validate a projection's exact output row type without executing it.
pub(in crate::sql) fn analyze_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<RowSchema, SQLError> {
    projection_output_schema(
        SchemaScope::for_analysis(ctes)?,
        routines,
        projections,
        expression_schema,
        star_schema,
        subqueries,
        params,
    )
}

fn projection_output_schema(
    mut scope: SchemaScope,
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
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
            routines,
            &projection.expr,
            expression_schema,
            subqueries,
            params,
            Some(expression_schema),
        )?);
    }
    Ok(RowSchema::with_types(columns, types))
}

/// Validate every scalar expression in a query block while the physical input still carries declared SQL types. This must precede polymorphic rewrites such as `pg_typeof`, because an invalid common type is an error, not an `unknown` result.
pub(in crate::sql) fn validate_query_block_expression_types(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let scalar_subquery_types = statement
        .subqueries
        .iter()
        .map(|plan| {
            bind_query_plan_schema(routines, plan, params, ctes, Some(schema))
                .map(|output| output.column_type(0).cloned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let resolver = QueryFunctionTypeResolver {
        routines,
        scalar_subquery_types: Some(scalar_subquery_types),
        defer_routine_namespace_errors: true,
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

/// Validate every query-block reference only after the caller has the authoritative source schema. This preserves registered table-function row shapes and checks recursive argument references before definitive routine namespace lookup.
pub(in crate::sql) fn validate_query_block_references(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let output = analyze_projection_output_schema(
        routines,
        &statement.projections,
        schema,
        schema,
        &statement.subqueries,
        params,
        ctes,
    )?;
    SchemaScope::for_analysis(ctes)?
        .validate_query_block_clauses(routines, statement, schema, &output, params)
}

pub(super) fn projection_star_columns(
    expression: &ScalarExpr,
    schema: &RowSchema,
) -> Result<Option<Vec<ProjectionStarColumn>>, SQLError> {
    match expression {
        ScalarExpr::Star => Ok(Some(
            schema
                .columns()
                .iter()
                .enumerate()
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

pub(super) fn rename_schema(
    schema: &RowSchema,
    aliases: &[String],
    qualifier: Option<&str>,
) -> RowSchema {
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            aliases
                .get(position)
                .cloned()
                .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
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
