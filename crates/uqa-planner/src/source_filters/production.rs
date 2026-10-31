//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Equality propagation borrows source terms and admits each selected destination owner.

use super::{expr_is_constant, expr_is_qualified_column};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    ast::BinaryOp,
    plan::{source_projection::QualifierFilters, OwnedScalarExpr, SourcePlan},
    semantics::{visit_filter_parts_with_control, visit_source_qualifiers_with_control},
    SQLError, ScalarExpr,
};

pub fn propagated_join_filters_with_control(
    filters: &QualifierFilters,
    source: &SourcePlan,
    target: &SourcePlan,
    on: Option<&ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<Option<QualifierFilters>, SQLError> {
    control.check()?;
    let Some(on) = on else {
        return Ok(None);
    };
    let mut source_qualifiers = ProductionMap::new(*control);
    let mut target_qualifiers = ProductionMap::new(*control);
    visit_source_qualifiers_with_control(source, control, &mut |name| {
        source_qualifiers.insert_copy(name, ())?;
        Ok(())
    })?;
    visit_source_qualifiers_with_control(target, control, &mut |name| {
        target_qualifiers.insert_copy(name, ())?;
        Ok(())
    })?;
    let mut output = None;
    visit_filter_parts_with_control(on, control, &mut |part| {
        let ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs,
            rhs,
        } = part
        else {
            return Ok(());
        };
        let (Some(left), Some(right)) = (column_pair(lhs), column_pair(rhs)) else {
            return Ok(());
        };
        for (source, target) in [(left, right), (right, left)] {
            if source_qualifiers.get(source.0).is_none()
                || target_qualifiers.get(target.0).is_none()
            {
                continue;
            }
            if let Some(predicates) = filters.get(source.0) {
                for predicate in predicates {
                    control.check()?;
                    let Some(value) = constant_for_column(predicate, source.0, source.1) else {
                        continue;
                    };
                    if output.is_none() {
                        output = Some(filters.copy_with_control(control)?);
                    }
                    let left = OwnedScalarExpr::column(
                        control.copy_text(target.1)?,
                        Some(control.copy_text(target.0)?),
                        control,
                    )?;
                    let predicate = OwnedScalarExpr::binary(
                        BinaryOp::Equal,
                        left,
                        value.copy_for_rewrite(control)?,
                        control,
                    )?;
                    output
                        .as_mut()
                        .expect("selected propagation")
                        .push_owned(target.0, predicate, control)?;
                }
            }
        }
        Ok(())
    })?;
    Ok(output)
}
fn column_pair(expression: &ScalarExpr) -> Option<(&str, &str)> {
    match expression {
        ScalarExpr::QualifiedColumn { qualifier, column } => Some((qualifier, column)),
        _ => None,
    }
}
fn constant_for_column<'a>(
    expression: &'a ScalarExpr,
    qualifier: &str,
    column: &str,
) -> Option<&'a ScalarExpr> {
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    if expr_is_qualified_column(lhs, qualifier, column) && expr_is_constant(rhs) {
        return Some(rhs);
    }
    if expr_is_qualified_column(rhs, qualifier, column) && expr_is_constant(lhs) {
        return Some(lhs);
    }
    None
}
