//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection labels, star expansion, and DML row-image schemas.

use super::{DOC_ID_COLUMN, TABLE_OID_COLUMN};
use crate::ast::ReturningAliases;
use crate::plan::ProjectionPlan;
use crate::{ColumnIdentity, RowSchema, SQLError, ScalarExpr};

pub fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
    projections.iter().map(projection_label_at).collect()
}

/// Compute a projection's `PostgreSQL` output column name. Standalone expressions use `?column?`; repeated labels remain repeated until the final named-map compatibility boundary.
pub fn projection_label_at(proj: &ProjectionPlan) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        ScalarExpr::Column(c) => c.clone(),
        ScalarExpr::QualifiedColumn { column, .. } => column.clone(),
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) => "*".into(),
        ScalarExpr::Func { name, .. } => crate::parse_regobject_name(name)
            .and_then(|mut names| names.pop())
            .unwrap_or_else(|| name.clone()),
        _ => "?column?".into(),
    }
}

pub fn expand_from_star_columns(
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    source_schema: &RowSchema,
) -> Result<Vec<String>, SQLError> {
    let mut output = Vec::new();
    for (position, projection) in projections.iter().enumerate() {
        match &projection.expr {
            ScalarExpr::Star => {
                output.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            visible_projection_source_position(source_schema, *position)
                        })
                        .map(|(source_position, column)| {
                            source_schema
                                .public_name(source_position)
                                .unwrap_or(column)
                                .to_string()
                        }),
                );
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let qualified_columns = source_schema
                    .qualified_star_position_layout(qualifier)
                    .into_iter()
                    .filter(|(_, logical, _, _)| {
                        logical.is_none_or(|position| {
                            visible_projection_source_position(source_schema, position)
                        })
                    })
                    .map(|(column, _, _, _)| column)
                    .collect::<Vec<_>>();
                if qualified_columns.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                output.extend(qualified_columns);
            }
            _ => output.push(columns[position].clone()),
        }
    }
    Ok(output)
}

pub fn bound_projection_expression(schema: &RowSchema, position: usize) -> ScalarExpr {
    let Some(identity) = schema.identity(position) else {
        return ScalarExpr::Position(position);
    };
    if let Some(qualifier) = identity.qualifier() {
        if schema.qualified_position(qualifier, identity.column()) == Some(position) {
            return ScalarExpr::qualified_column(qualifier, identity.column());
        }
    } else if schema.unqualified_position(identity.column()) == Some(position) {
        return ScalarExpr::Column(identity.column().to_string());
    }
    ScalarExpr::Position(position)
}

pub fn expand_bound_projection_stars(
    projections: &[ProjectionPlan],
    schema: &RowSchema,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in projections {
        match &projection.expr {
            ScalarExpr::Star => {
                for (position, column) in schema.columns().iter().enumerate() {
                    if !visible_projection_source_position(schema, position) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: bound_projection_expression(schema, position),
                        alias: Some(schema.public_name(position).unwrap_or(column).to_string()),
                    });
                }
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let layout = schema.qualified_star_position_layout(qualifier);
                if layout.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                for (column, logical, _, _) in layout {
                    if logical.is_some_and(|position| {
                        !visible_projection_source_position(schema, position)
                    }) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: logical.map_or_else(
                            || ScalarExpr::qualified_column(qualifier, &column),
                            |position| bound_projection_expression(schema, position),
                        ),
                        alias: Some(column),
                    });
                }
            }
            _ => expanded.push(projection.clone()),
        }
    }
    Ok(expanded)
}

pub fn visible_projection_source_position(schema: &RowSchema, position: usize) -> bool {
    schema.wildcard_position_visible(position)
}

pub fn returning_context_schema(
    columns: &[String],
    types: &[Option<crate::ast::ColumnType>],
    composite_width: usize,
    target_qualifier: &str,
    aliases: &ReturningAliases,
) -> RowSchema {
    let target =
        RowSchema::with_qualified_types(target_qualifier, columns.to_vec(), types.to_vec());
    let target = RowSchema::with_wildcard_hidden_positions(&target, composite_width..columns.len());
    let hidden_types = types
        .iter()
        .cloned()
        .chain(types.iter().cloned())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&target, &hidden_types);
    let width = columns.len();
    let identity_aliases = columns
        .iter()
        .enumerate()
        .flat_map(|(position, column)| {
            [
                (
                    ColumnIdentity::qualified(&aliases.old, column),
                    width + position,
                    types[position].clone(),
                ),
                (
                    ColumnIdentity::qualified(&aliases.new, column),
                    width * 2 + position,
                    types[position].clone(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    RowSchema::with_physical_identity_aliases(&schema, &identity_aliases)
}

pub fn returning_expression_schema(
    target: &RowSchema,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let composite_width = target.len();
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(crate::ast::ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(crate::ast::ColumnType::Oid));
    columns.push(crate::semantics::XMIN_COLUMN.into());
    types.push(Some(crate::ast::ColumnType::Xid));
    let target =
        returning_context_schema(&columns, &types, composite_width, target_qualifier, aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

pub fn query_plan_output_columns(plan: &crate::plan::QueryPlan) -> Option<Vec<String>> {
    match &plan.root {
        crate::plan::RelationalPlan::QueryBlock(block) => {
            Some(projection_columns(&block.projections))
        }
        crate::plan::RelationalPlan::SetOp { left, .. } => query_plan_output_columns(left),
        crate::plan::RelationalPlan::Values { rows, .. } => rows.first().map(|row| {
            (1..=row.len())
                .map(|index| format!("column{index}"))
                .collect()
        }),
    }
}

pub fn should_defer_distinct_limit(stmt: &crate::plan::QueryBlockPlan) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

pub fn select_execution_stmt(
    stmt: &crate::plan::QueryBlockPlan,
    defer_distinct_limit: bool,
) -> crate::plan::QueryBlockPlan {
    if !defer_distinct_limit {
        return stmt.clone();
    }
    let mut exec_stmt = stmt.clone();
    exec_stmt.limit = None;
    exec_stmt.offset = None;
    exec_stmt
}
