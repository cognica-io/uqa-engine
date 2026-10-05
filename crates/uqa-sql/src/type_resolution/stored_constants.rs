//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored expressions keep the enum constants that parse analysis coerces from `unknown` literals by label identity, as `PostgreSQL` stores `Const` nodes: `ALTER TYPE ... RENAME VALUE` changes their label and never their meaning, and reloading the catalog never converts label text again. Binding decides every coercion; this module carries the constants it produced back into the stored tree. Binding changes a stored tree only by adding casts, binding calls and converting literals, so the two trees correspond node for node once the added casts are skipped.

use super::{is_unknown_literal, FunctionTypeResolver};
use crate::ast::ColumnType;
use crate::expr::enums::{fold_unknown_literal, is_enum_bearing, EnumLabelCatalog};
use crate::schema::ScalarTypeSchema;
use crate::{SQLError, SQLParam, ScalarExpr, ScalarFrameBound};
use uqa_core::Value;

/// Whether an expression has an `unknown` literal outside its subqueries, the only nodes that binding converts to enum constants.
#[must_use]
pub fn contains_unknown_literal(expression: &ScalarExpr) -> bool {
    let mut found = false;
    expression.visit(&mut |node| {
        found |= is_unknown_literal(node);
    });
    found
}

/// Replace every `unknown` literal that binding coerces to an enum, an enum array or a domain over one by the constant binding produced for it. Returns whether any literal changed.
pub fn fold_stored_enum_constants(
    expression: &mut ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let Some(catalog) = resolver.enum_labels() else {
        return Ok(false);
    };
    let Some(bound) =
        super::introspection::bind_catalog_constants(expression, schema, params, resolver)?
    else {
        return Ok(false);
    };
    transfer(expression, &bound, catalog)
}

/// Casts that binding added around a node are not part of the stored tree. A cast the stored tree already has keeps its written type name, which binding never changes.
fn without_added_casts<'a>(stored: &ScalarExpr, mut bound: &'a ScalarExpr) -> &'a ScalarExpr {
    while let ScalarExpr::Cast { expr, ty } = bound {
        if matches!(stored, ScalarExpr::Cast { ty: written, .. } if written == ty) {
            break;
        }
        bound = expr;
    }
    bound
}

#[expect(
    clippy::too_many_lines,
    reason = "one structural walk pairs every scalar variant with its bound counterpart"
)]
fn transfer(
    stored: &mut ScalarExpr,
    bound: &ScalarExpr,
    catalog: &dyn EnumLabelCatalog,
) -> Result<bool, SQLError> {
    let bound = without_added_casts(stored, bound);
    if is_unknown_literal(stored) {
        let ScalarExpr::TypedLiteral {
            bound_type: Some(target),
            ..
        } = bound
        else {
            return Ok(false);
        };
        return fold_literal(stored, target, catalog);
    }
    Ok(match (stored, bound) {
        // An explicit cast of a literal whose conversion binding folded: the constant replaces the literal and the cast remains, converting a value of its own type.
        (ScalarExpr::Cast { expr, .. }, ScalarExpr::TypedLiteral { .. }) => {
            transfer(expr, bound, catalog)?
        }
        (ScalarExpr::Cast { expr, .. }, ScalarExpr::Cast { expr: bound, .. })
        | (ScalarExpr::UnaryMinus(expr), ScalarExpr::UnaryMinus(bound))
        | (ScalarExpr::Not(expr), ScalarExpr::Not(bound))
        | (ScalarExpr::IsNull { expr, .. }, ScalarExpr::IsNull { expr: bound, .. })
        | (ScalarExpr::InSubquery { expr, .. }, ScalarExpr::InSubquery { expr: bound, .. }) => {
            transfer(expr, bound, catalog)?
        }
        (
            ScalarExpr::Func {
                name,
                args,
                order_by,
                filter,
                ..
            },
            ScalarExpr::Func {
                name: bound_name,
                args: bound_args,
                order_by: bound_order,
                filter: bound_filter,
                ..
            },
        ) => {
            // Binding may replace a call by another with rearranged arguments; such a call keeps its literals.
            if name != bound_name
                || args.len() != bound_args.len()
                || order_by.len() != bound_order.len()
                || filter.is_some() != bound_filter.is_some()
            {
                return Ok(false);
            }
            let mut changed = transfer_all(args, bound_args, catalog)?;
            for (order, bound) in order_by.iter_mut().zip(bound_order) {
                changed |= transfer(&mut order.expr, &bound.expr, catalog)?;
            }
            if let (Some(filter), Some(bound)) = (filter, bound_filter) {
                changed |= transfer(filter, bound, catalog)?;
            }
            changed
        }
        (ScalarExpr::Array(items), ScalarExpr::Array(bound))
        | (ScalarExpr::Row(items), ScalarExpr::Row(bound))
        | (ScalarExpr::And(items), ScalarExpr::And(bound))
        | (ScalarExpr::Or(items), ScalarExpr::Or(bound)) => transfer_all(items, bound, catalog)?,
        (
            ScalarExpr::Binary { lhs, rhs, .. },
            ScalarExpr::Binary {
                lhs: bound_lhs,
                rhs: bound_rhs,
                ..
            },
        ) => transfer(lhs, bound_lhs, catalog)? | transfer(rhs, bound_rhs, catalog)?,
        (
            ScalarExpr::Between { expr, low, high },
            ScalarExpr::Between {
                expr: bound_expr,
                low: bound_low,
                high: bound_high,
            },
        ) => {
            transfer(expr, bound_expr, catalog)?
                | transfer(low, bound_low, catalog)?
                | transfer(high, bound_high, catalog)?
        }
        (
            ScalarExpr::InList { expr, list, .. },
            ScalarExpr::InList {
                expr: bound_expr,
                list: bound_list,
                ..
            },
        ) => transfer(expr, bound_expr, catalog)? | transfer_all(list, bound_list, catalog)?,
        (
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                ..
            },
            ScalarExpr::WindowCall {
                name: bound_name,
                args: bound_args,
                spec: bound_spec,
                filter: bound_filter,
                ..
            },
        ) => {
            if name != bound_name
                || filter.is_some() != bound_filter.is_some()
                || args.len() != bound_args.len()
                || spec.partition_by.len() != bound_spec.partition_by.len()
                || spec.order_by.len() != bound_spec.order_by.len()
            {
                return Ok(false);
            }
            let mut changed = transfer_all(args, bound_args, catalog)?;
            if let (Some(filter), Some(bound)) = (filter, bound_filter) {
                changed |= transfer(filter, bound, catalog)?;
            }
            changed |= transfer_all(&mut spec.partition_by, &bound_spec.partition_by, catalog)?;
            for (order, bound) in spec.order_by.iter_mut().zip(&bound_spec.order_by) {
                changed |= transfer(&mut order.expr, &bound.expr, catalog)?;
            }
            if let (Some(frame), Some(bound)) = (spec.frame.as_mut(), bound_spec.frame.as_ref()) {
                changed |= transfer_frame_bound(&mut frame.start, &bound.start, catalog)?;
                changed |= transfer_frame_bound(&mut frame.end, &bound.end, catalog)?;
            }
            changed
        }
        (
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            },
            ScalarExpr::Case {
                base: bound_base,
                when: bound_when,
                else_branch: bound_else,
            },
        ) => {
            if base.is_some() != bound_base.is_some()
                || when.len() != bound_when.len()
                || else_branch.is_some() != bound_else.is_some()
            {
                return Ok(false);
            }
            let mut changed = false;
            if let (Some(base), Some(bound)) = (base, bound_base) {
                changed |= transfer(base, bound, catalog)?;
            }
            for ((condition, result), (bound_condition, bound_result)) in
                when.iter_mut().zip(bound_when)
            {
                changed |= transfer(condition, bound_condition, catalog)?;
                changed |= transfer(result, bound_result, catalog)?;
            }
            if let (Some(branch), Some(bound)) = (else_branch, bound_else) {
                changed |= transfer(branch, bound, catalog)?;
            }
            changed
        }
        _ => false,
    })
}

fn transfer_all(
    stored: &mut [ScalarExpr],
    bound: &[ScalarExpr],
    catalog: &dyn EnumLabelCatalog,
) -> Result<bool, SQLError> {
    if stored.len() != bound.len() {
        return Ok(false);
    }
    let mut changed = false;
    for (stored, bound) in stored.iter_mut().zip(bound) {
        changed |= transfer(stored, bound, catalog)?;
    }
    Ok(changed)
}

fn transfer_frame_bound(
    stored: &mut ScalarFrameBound,
    bound: &ScalarFrameBound,
    catalog: &dyn EnumLabelCatalog,
) -> Result<bool, SQLError> {
    match (stored, bound) {
        (ScalarFrameBound::Preceding(stored), ScalarFrameBound::Preceding(bound))
        | (ScalarFrameBound::Following(stored), ScalarFrameBound::Following(bound)) => {
            transfer(stored, bound, catalog)
        }
        _ => Ok(false),
    }
}

/// Convert the stored literal itself with the type binding chose for it, so every constant carries the label its own text names.
fn fold_literal(
    stored: &mut ScalarExpr,
    target: &ColumnType,
    catalog: &dyn EnumLabelCatalog,
) -> Result<bool, SQLError> {
    if !is_enum_bearing(target) {
        return Ok(false);
    }
    let ScalarExpr::Literal(value) = stored else {
        return Ok(false);
    };
    let Some(value) = fold_unknown_literal(Some(catalog), value, target)? else {
        return Ok(false);
    };
    *stored = stored_enum_constant(value, target);
    Ok(true)
}

/// The stored form of an enum constant: its label-identity value under the type's OID identity. The type is resolved from the identity when the expression is bound, so no cached name can go stale.
#[must_use]
pub fn stored_enum_constant(value: Value, target: &ColumnType) -> ScalarExpr {
    ScalarExpr::TypedLiteral {
        value,
        ty: target.catalog_name(),
        bound_type: None,
        parameter_index: None,
    }
}
