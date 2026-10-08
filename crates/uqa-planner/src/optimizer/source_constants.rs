//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate admitted constants from VALUES and source-free subqueries, preserving empty-input grouping.

use std::{borrow::Cow, collections::BTreeMap};

use crate::{QueryBlockPlan, RelationalPlan, SourcePlan};
use uqa_sql::routines::declaration::RoutineTypeCatalog;
use uqa_sql::{ColumnType, ScalarExpr};

type ConstantRow<'a> = (Cow<'a, [ScalarExpr]>, Cow<'a, [String]>);

pub(super) fn propagate_source_constants(
    block: &mut QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
) {
    let Some(source) = &block.from else { return };
    let Some((row, columns)) = single_constant_row(source, types) else {
        return;
    };
    let qualifier = source.visible_qualifier().map(str::to_string);
    let mut constants = BTreeMap::<String, Option<ScalarExpr>>::new();
    for (index, value) in row.iter().enumerate() {
        let name = columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("column{}", index + 1));
        let value = constant_literal(value, types).then(|| match value {
            ScalarExpr::Cast { expr, .. } => expr.as_ref().clone(),
            value => value.clone(),
        });
        constants
            .entry(name)
            .and_modify(|value| *value = None)
            .or_insert(value);
    }
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
                projection.alias = Some(column.clone());
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
}

fn single_constant_row<'a>(
    source: &'a SourcePlan,
    types: Option<&dyn RoutineTypeCatalog>,
) -> Option<ConstantRow<'a>> {
    match source {
        SourcePlan::Values {
            rows,
            column_aliases,
            internal_relation: None,
            ..
        } if rows.len() == 1 => Some((Cow::Borrowed(&rows[0]), Cow::Borrowed(column_aliases))),
        SourcePlan::Subquery {
            body,
            column_aliases,
            ..
        } if body.ctes.is_empty() => match &body.root {
            RelationalPlan::Values { rows, subqueries }
                if rows.len() == 1 && subqueries.is_empty() =>
            {
                Some((Cow::Borrowed(&rows[0]), Cow::Borrowed(column_aliases)))
            }
            RelationalPlan::QueryBlock(inner)
                if inner.from.is_none()
                    && inner
                        .projections
                        .iter()
                        .all(|p| constant_literal(&p.expr, types)) =>
            {
                let columns = inner
                    .projections
                    .iter()
                    .enumerate()
                    .map(|(i, projection)| {
                        column_aliases
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| uqa_sql::semantics::projection_label_at(projection))
                    })
                    .collect();
                Some((
                    Cow::Owned(inner.projections.iter().map(|p| p.expr.clone()).collect()),
                    Cow::Owned(columns),
                ))
            }
            _ => None,
        },
        _ => None,
    }
}

fn constant_literal(expr: &ScalarExpr, types: Option<&dyn RoutineTypeCatalog>) -> bool {
    match expr {
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
    for expression in block.expressions() {
        expression.visit(&mut |node| {
            if let ScalarExpr::Func { name, .. } = node {
                grouping |= name.eq_ignore_ascii_case("grouping");
            }
        });
    }
    if !grouping {
        block.group_by = vec![ScalarExpr::Literal(uqa_core::Value::Bool(true))];
    }
}
