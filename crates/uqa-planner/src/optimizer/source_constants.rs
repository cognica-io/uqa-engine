//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate admitted constants from VALUES and source-free subqueries, preserving empty-input grouping.

use std::collections::BTreeMap;

use crate::{QueryBlockPlan, RelationalPlan, SourcePlan};
use uqa_sql::routines::declaration::RoutineTypeCatalog;
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

type Constants = BTreeMap<String, Option<ScalarExpr>>;

#[cfg(test)]
mod tests;

pub(super) fn propagate_source_constants(
    block: &mut QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
) -> Result<(), SQLError> {
    let Some(source) = &block.from else {
        return Ok(());
    };
    let Some(constants) = source_constants(source, types, views)? else {
        return Ok(());
    };
    let qualifier = source.visible_qualifier().map(str::to_string);
    let mut replace = |expression: &mut ScalarExpr| {
        let column = match expression {
            ScalarExpr::Column(column) => Some(column.as_str()),
            ScalarExpr::QualifiedColumn {
                qualifier: requested,
                column,
            } if qualifier.as_deref() == Some(requested.as_str()) => Some(column.as_str()),
            _ => None,
        };
        if let Some(Some(value)) = column.and_then(|column| constants.get(column)) {
            *expression = super::retain_computed_integer(value.clone(), None);
        }
    };
    let rewrite = crate::unified_plan::rewrite_scalar_expression;
    for projection in &mut block.projections {
        if projection.alias.is_none() {
            if let ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } =
                &projection.expr
            {
                projection.alias = Some(column.to_string());
            }
        }
        rewrite(&mut projection.expr, &mut replace);
    }
    for expression in block
        .r#where
        .iter_mut()
        .chain(&mut block.group_by)
        .chain(block.grouping_sets.iter_mut().flatten())
        .chain(block.having.iter_mut())
        .chain(block.order_by.iter_mut().map(|order| &mut order.expr))
        .chain(block.limit.iter_mut())
        .chain(block.offset.iter_mut())
        .chain(&mut block.distinct_on)
        .chain(
            block
                .windows
                .iter_mut()
                .flat_map(|window| window.spec.expressions_mut()),
        )
    {
        rewrite(expression, &mut replace);
    }
    Ok(())
}

fn source_constants(
    source: &SourcePlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
) -> Result<Option<Constants>, SQLError> {
    match source {
        SourcePlan::Table {
            name,
            bound_columns,
            column_aliases,
            ..
        } => {
            let Some(views) = views else {
                return Ok(None);
            };
            let mut constants = None;
            views.with_view_query(name, &mut |body| {
                // Inspect outputs without replacing the view source: authorization, cardinality and dependency observations still execute normally.
                constants = query_constants(
                    body,
                    |index| {
                        column_aliases
                            .get(index)
                            .or_else(|| bound_columns.as_ref().and_then(|names| names.get(index)))
                    },
                    types,
                );
                Ok(constants.is_some())
            })?;
            Ok(constants)
        }
        SourcePlan::Values {
            rows,
            column_aliases,
            internal_relation: None,
            ..
        } if rows.len() == 1 => Ok(Some(constants_from_values(
            &rows[0],
            |index| value_column_name(column_aliases.get(index), index),
            types,
        ))),
        SourcePlan::Subquery {
            captured_output: None,
            body,
            column_aliases,
            ..
        } => Ok(query_constants(
            body,
            |index| column_aliases.get(index),
            types,
        )),
        _ => Ok(None),
    }
}

fn query_constants<'a>(
    body: &uqa_sql::plan::QueryPlan,
    column_name: impl Fn(usize) -> Option<&'a String>,
    types: Option<&dyn RoutineTypeCatalog>,
) -> Option<Constants> {
    if !body.ctes.is_empty() {
        return None;
    }
    match &body.root {
        RelationalPlan::Values { rows, subqueries } if rows.len() == 1 && subqueries.is_empty() => {
            Some(constants_from_values(
                &rows[0],
                |index| value_column_name(column_name(index), index),
                types,
            ))
        }
        RelationalPlan::QueryBlock(inner)
            if inner.from.is_none()
                && inner
                    .projections
                    .iter()
                    .all(|p| constant_literal(&p.expr, types)) =>
        {
            Some(constants_from_values(
                inner.projections.iter().map(|projection| &projection.expr),
                |index| {
                    column_name(index).cloned().unwrap_or_else(|| {
                        uqa_sql::semantics::projection_label_at(&inner.projections[index])
                    })
                },
                types,
            ))
        }
        _ => None,
    }
}

fn value_column_name(name: Option<&String>, index: usize) -> String {
    name.cloned()
        .unwrap_or_else(|| format!("column{}", index + 1))
}

fn constants_from_values<'a>(
    values: impl IntoIterator<Item = &'a ScalarExpr>,
    column_name: impl Fn(usize) -> String,
    types: Option<&dyn RoutineTypeCatalog>,
) -> Constants {
    let mut constants = Constants::new();
    for (index, value) in values.into_iter().enumerate() {
        let name = column_name(index);
        let value = constant_literal(value, types).then(|| match value {
            // SELECT targets and one-row VALUES resolve unknown literals to text before an outer query can observe their type. Substitution must preserve that boundary.
            ScalarExpr::Literal(value @ (uqa_core::Value::Null | uqa_core::Value::Str(_))) => {
                ScalarExpr::TypedLiteral {
                    collation_oid: None,
                    composite_source: None,
                    value: value.clone(),
                    ty: "text".into(),
                    bound_type: Some(ColumnType::Text),
                    parameter_index: None,
                }
            }
            ScalarExpr::Cast { expr, .. } => expr.as_ref().clone(),
            value => value.clone(),
        });
        constants
            .entry(name)
            .and_modify(|value| *value = None)
            .or_insert(value);
    }
    constants
}

fn constant_literal(expr: &ScalarExpr, types: Option<&dyn RoutineTypeCatalog>) -> bool {
    match expr {
        ScalarExpr::Collated { expr, .. } => constant_literal(expr, types),
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral {
            parameter_index: None,
            ..
        } => true,
        ScalarExpr::Cast { expr, ty, .. } => {
            let ScalarExpr::TypedLiteral {
                ty: original,
                bound_type,
                parameter_index: None,
                ..
            } = expr.as_ref()
            else {
                return false;
            };
            original == ty || match (bound_type, types) {
                (Some(ColumnType::Composite(reference)), Some(types)) => types.resolve_catalog_column_type_name(ty).is_ok_and(|target| matches!(target, ColumnType::Composite(target) if target.oid == reference.oid)),
                _ => false,
            }
        }
        _ => false,
    }
}

/// An admitted constant key defines a single equivalence class without invoking its type's equality or hash functions. Retain a key so an empty input still produces no group.
pub(super) fn simplify_constant_group(
    block: &mut QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
) {
    if block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || !block
            .group_by
            .iter()
            .all(|expr| constant_literal(expr, types))
    {
        return;
    }
    let mut grouping = false;
    block.visit_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            if let ScalarExpr::Func { name, .. } = node {
                grouping |= name.eq_ignore_ascii_case("grouping");
            }
        });
    });
    if !grouping {
        block.group_by = vec![ScalarExpr::Literal(uqa_core::Value::Bool(true))];
    }
}
