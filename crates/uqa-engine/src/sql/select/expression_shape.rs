//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal, qualifier analysis, and safe qualification.

use super::{collect_from_qualifiers, BTreeSet, ScalarExpr, SourcePlan};

pub(in crate::sql) fn expr_contains_function(expression: &ScalarExpr) -> bool {
    let mut contains_function = false;
    expression.visit(&mut |part| {
        contains_function |= matches!(
            part,
            ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. }
        );
    });
    contains_function
}

pub(in crate::sql) fn flatten_and_filter_parts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(flatten_and_filter_parts).collect(),
        other => vec![other],
    }
}

pub(in crate::sql) fn from_qualifier_set(from: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

pub(in crate::sql) fn expr_qualifiers(expr: &ScalarExpr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

pub(in crate::sql) fn collect_expr_qualifiers(
    expr: &ScalarExpr,
    qualifiers: &mut BTreeSet<String>,
) {
    expr.visit(&mut |part| match part {
        ScalarExpr::QualifiedColumn { qualifier, .. } | ScalarExpr::QualifiedStar(qualifier) => {
            qualifiers.insert(qualifier.clone());
        }
        _ => {}
    });
}

pub(in crate::sql) fn expr_has_unqualified_column(expr: &ScalarExpr) -> bool {
    let mut found = false;
    expr.visit(&mut |part| {
        found |= matches!(part, ScalarExpr::Column(_));
    });
    found
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn qualify_unqualified_columns(
    expr: &ScalarExpr,
    qualifier: &str,
) -> ScalarExpr {
    match expr {
        ScalarExpr::Column(column) => ScalarExpr::qualified_column(qualifier, column),
        ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star => expr.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Row(items) => ScalarExpr::Row(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(qualify_unqualified_columns(lhs, qualifier)),
            rhs: Box::new(qualify_unqualified_columns(rhs, qualifier)),
        },
        ScalarExpr::Not(inner) => {
            ScalarExpr::Not(Box::new(qualify_unqualified_columns(inner, qualifier)))
        }
        ScalarExpr::UnaryMinus(inner) => {
            ScalarExpr::UnaryMinus(Box::new(qualify_unqualified_columns(inner, qualifier)))
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            low: Box::new(qualify_unqualified_columns(low, qualifier)),
            high: Box::new(qualify_unqualified_columns(high, qualifier)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            list: list
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
            negated: *negated,
        },
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_ref()
                .map(|filter| Box::new(qualify_unqualified_columns(filter, qualifier))),
        },
        ScalarExpr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            spec: spec.clone(),
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
            when: when
                .iter()
                .map(|(cond, result)| {
                    (
                        qualify_unqualified_columns(cond, qualifier),
                        qualify_unqualified_columns(result, qualifier),
                    )
                })
                .collect(),
            else_branch: else_branch
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            ty: ty.clone(),
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => expr.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{expr_contains_function, expr_has_unqualified_column, expr_qualifiers, ScalarExpr};
    use uqa_execution::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};
    use uqa_sql::ast::FrameMode;

    #[test]
    fn expression_shape_uses_complete_scalar_traversal() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::QualifiedColumn {
                qualifier: "orders".into(),
                column: "amount".into(),
            }],
            spec: ScalarWindowSpec {
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Column(
                        "frame_width".into(),
                    ))),
                    end: ScalarFrameBound::CurrentRow,
                }),
            },
        };
        assert!(expr_contains_function(&expression));
        assert!(expr_has_unqualified_column(&expression));
        assert_eq!(
            expr_qualifiers(&expression),
            std::collections::BTreeSet::from(["orders".into()])
        );
    }
}
