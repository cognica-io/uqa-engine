//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate constant columns from a single-row VALUES source into its consumer.

use std::collections::BTreeMap;

use crate::{QueryBlockPlan, RelationalPlan, SourcePlan};
use uqa_sql::ScalarExpr;

pub(super) fn propagate_source_constants(block: &mut QueryBlockPlan) {
    let Some(source) = &block.from else { return };
    let Some((row, columns)) = single_values_row(source) else {
        return;
    };
    let qualifier = source.visible_qualifier().map(str::to_string);
    let mut constants = BTreeMap::<String, Option<ScalarExpr>>::new();
    for (index, value) in row.iter().enumerate() {
        let name = columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("column{}", index + 1));
        let value = matches!(
            value,
            ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
        )
        .then(|| value.clone());
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
            *expression = value.clone();
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
    {
        rewrite(expression, &mut replace);
    }
}

fn single_values_row(source: &SourcePlan) -> Option<(&[ScalarExpr], &[String])> {
    match source {
        SourcePlan::Values {
            rows,
            column_aliases,
            internal_relation: None,
            ..
        } if rows.len() == 1 => Some((&rows[0], column_aliases)),
        SourcePlan::Subquery {
            body,
            column_aliases,
            ..
        } if body.ctes.is_empty() => match &body.root {
            RelationalPlan::Values { rows, subqueries }
                if rows.len() == 1 && subqueries.is_empty() =>
            {
                Some((&rows[0], column_aliases))
            }
            _ => None,
        },
        _ => None,
    }
}
