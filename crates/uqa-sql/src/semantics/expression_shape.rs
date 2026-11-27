//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal, qualifier analysis, and safe qualification.

mod qualification;
pub use qualification::qualify_unqualified_columns_with_control;

use super::collect_from_qualifiers;
use crate::plan::SourcePlan;
use crate::ScalarExpr;
use std::collections::BTreeSet;

pub fn expr_contains_function(expression: &ScalarExpr) -> bool {
    expression.any_node(|part| {
        matches!(
            part,
            ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. }
        )
    })
}

pub fn flatten_and_filter_parts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    let mut parts = Vec::new();
    visit_filter_parts_with_control(
        expr,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut |part| {
            parts.push(part);
            Ok(())
        },
    )
    .expect("ordinary conjunction traversal");
    parts
}

pub fn from_qualifier_set(from: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

pub fn expr_qualifiers(expr: &ScalarExpr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

pub fn collect_expr_qualifiers(expr: &ScalarExpr, qualifiers: &mut BTreeSet<String>) {
    expr.visit(&mut |part| {
        if let Some(qualifier) = part.reference_qualifier() {
            qualifiers.insert(qualifier.to_owned());
        }
    });
}

pub fn expr_has_unqualified_column(expr: &ScalarExpr) -> bool {
    expr.any_node(|part| matches!(part, ScalarExpr::Column(_)))
}

pub fn qualify_unqualified_columns(expr: &ScalarExpr, qualifier: &str) -> ScalarExpr {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    qualify_unqualified_columns_with_control(expr, qualifier, &control)
        .expect("ordinary column qualification")
        .finish(&control)
        .expect("ordinary qualified scalar")
        .into_uncontrolled()
        .expect("ordinary qualified scalar has no reservation")
}

/// True when the expression can never evaluate to SQL NULL for any
/// row: registered search functions, IS NULL tests, and boolean
/// combinations thereof. Anything referencing column comparisons may
/// yield NULL, so set-complement `NOT` would be unsound for it.
pub fn expr_is_null_free(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Func { name, .. } => crate::registry::is_registered(name),
        ScalarExpr::IsNull { .. } => true,
        ScalarExpr::Exists { .. } => true,
        ScalarExpr::Literal(v) => !matches!(v, uqa_core::Value::Null),
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => parts.iter().all(expr_is_null_free),
        ScalarExpr::Not(inner) => expr_is_null_free(inner),
        _ => false,
    }
}

/// Visit the existing conjunction partitions in source order without allocating a flattened vector.
pub fn visit_filter_parts_with_control<'a>(
    expression: &'a ScalarExpr,
    control: &uqa_core::memory::ProductionControl<'_>,
    visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), crate::SQLError>,
) -> Result<(), crate::SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::And(items) => {
            for item in items {
                visit_filter_parts_with_control(item, control, visit)?;
            }
            Ok(())
        }
        expression => visit(expression),
    }
}

#[cfg(test)]
mod tests {
    use super::{expr_contains_function, expr_has_unqualified_column, expr_qualifiers, ScalarExpr};
    use crate::ast::{FrameExclusion, FrameMode};
    use crate::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};

    #[test]
    fn expression_shape_uses_complete_scalar_traversal() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::QualifiedColumn {
                qualifier: "orders".into(),
                column: "amount".into(),
            }],
            spec: ScalarWindowSpec {
                definition: None,
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: Some(ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Column(
                        "frame_width".into(),
                    ))),
                    end: ScalarFrameBound::CurrentRow,
                    between: true,
                    exclusion: FrameExclusion::NoOthers,
                }),
            },
            filter: None,
            modifiers: crate::ast::WindowCallModifiers::default(),
        };
        assert!(expr_contains_function(&expression));
        assert!(expr_has_unqualified_column(&expression));
        assert_eq!(
            expr_qualifiers(&expression),
            std::collections::BTreeSet::from(["orders".into()])
        );
    }
}
