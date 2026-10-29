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

mod labels;
pub(crate) use labels::field_selection_label;
pub use labels::{
    function_projection_label, projection_label_at, projection_labels_with_control,
    visit_query_output_columns, with_projection_label, ProjectionLabels,
};

pub fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
    projections.iter().map(projection_label_at).collect()
}

/// Resolve an SQL92 output ordinal against the expanded SELECT list before using its type or physical expression.
pub fn query_output_position(position: i64, width: usize, clause: &str) -> Result<usize, SQLError> {
    usize::try_from(position)
        .ok()
        .and_then(|position| position.checked_sub(1))
        .filter(|position| *position < width)
        .ok_or_else(|| output_position_error(clause, position))
}

pub fn output_position_error(clause: &str, position: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P10".into(),
        message: format!("{clause} position {position} is not in select list"),
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
                if qualified_columns.is_empty() && !source_schema.has_qualifier(qualifier) {
                    return Err(SQLError::UnknownTable(qualifier.to_string()));
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
        return ScalarExpr::Column(identity.column().to_string().into());
    }
    ScalarExpr::Position(position)
}

pub fn expand_bound_projection_stars(
    projections: &[ProjectionPlan],
    schema: &RowSchema,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    expand_bound_projection_stars_in_scope(projections, schema, schema)
}

/// A qualified star in a ROW constructor expands its source fields; an ordinary whole-row reference remains one nested value.
pub(crate) fn row_star_fields(
    expression: &ScalarExpr,
    schema: &RowSchema,
) -> Result<Option<Vec<ScalarExpr>>, SQLError> {
    if !matches!(expression, ScalarExpr::QualifiedStar(_)) {
        return Ok(None);
    }
    expand_bound_projection_stars(
        &[ProjectionPlan {
            expr: expression.clone(),
            alias: None,
        }],
        schema,
    )
    .map(|fields| Some(fields.into_iter().map(|field| field.expr).collect()))
}

/// Bare stars expand the local FROM output, while qualified stars can select an admitted enclosing namespace.
pub fn expand_bound_projection_stars_in_scope(
    projections: &[ProjectionPlan],
    schema: &RowSchema,
    expression_schema: &RowSchema,
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
                let schema = expression_schema;
                schema.require_lateral_qualifier(qualifier)?;
                let layout = schema.qualified_star_position_layout(qualifier);
                if layout.is_empty() && !schema.has_qualifier(qualifier) {
                    if let Some(error) = schema.unavailable_qualifier_error(qualifier) {
                        return Err(error);
                    }
                    return Err(SQLError::UnknownTable(qualifier.to_string()));
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
    returning_schema_for_target(target, composite_width, aliases)
}

/// Add OLD/NEW image bindings to an already-qualified, positional target schema. Each image retains the target column's selected collation even when the image's value is NULL.
pub fn returning_schema_for_target(
    target: RowSchema,
    composite_width: usize,
    aliases: &ReturningAliases,
) -> RowSchema {
    let width = target.len();
    assert_eq!(
        target.physical_width(),
        width,
        "RETURNING target must be positional"
    );
    assert!(
        composite_width <= width,
        "RETURNING user columns must belong to the target"
    );
    debug_assert!((0..width).all(|position| target.slot(position) == Some(position)));
    let target = RowSchema::with_wildcard_hidden_positions(&target, composite_width..width);
    let types = target.column_types();
    let hidden_types = types
        .iter()
        .cloned()
        .chain(types.iter().cloned())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&target, &hidden_types);
    let identity_aliases = target
        .columns()
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
    let mut schema = RowSchema::with_physical_identity_aliases(&schema, &identity_aliases);
    for (binding, oid) in target.collation_overrides() {
        let crate::schema::CollationBinding::Column(position) = binding else {
            continue;
        };
        for qualifier in [&aliases.old, &aliases.new] {
            let identity = ColumnIdentity::qualified(qualifier, &target.columns()[position]);
            schema = schema
                .with_collation_override(crate::schema::CollationBinding::Alias(&identity), oid)
                .expect("RETURNING image alias retains its target's type");
        }
    }
    schema
}

pub fn returning_expression_schema(
    target: &RowSchema,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let composite_width = target.len();
    let (columns, types) = target_with_system_columns(target);
    let width = columns.len();
    let schema = RowSchema::with_qualified_types(target_qualifier, columns, types)
        .with_column_collations((0..width).map(|position| target.column_collation(position)));
    let target = returning_schema_for_target(schema, composite_width, aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

/// The row scope of the clauses of a data-modifying statement other than `RETURNING`: the target, named by its name or alias, with its system columns, and the statement's other sources. The `old` and `new` aliases of the target belong to `RETURNING` alone.
#[must_use]
pub fn mutation_clause_schema(
    target: &RowSchema,
    target_qualifier: &str,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let composite_width = target.len();
    let (columns, types) = target_with_system_columns(target);
    let width = columns.len();
    let target = RowSchema::with_wildcard_hidden_positions(
        &RowSchema::with_qualified_types(target_qualifier, columns, types)
            .with_column_collations((0..width).map(|position| target.column_collation(position))),
        composite_width..width,
    );
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

/// The target's columns followed by the system columns a statement can name on it.
fn target_with_system_columns(
    target: &RowSchema,
) -> (Vec<String>, Vec<Option<crate::ast::ColumnType>>) {
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
    (columns, types)
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
