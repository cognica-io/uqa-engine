//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Grouping identity observes existing metadata and admits only retained value decoding.

use crate::{ir::RetainedScalarLiteral, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

mod retained;

pub fn exprs_match_with_control(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    Ok(match (lhs, rhs) {
        (ScalarExpr::RetainedLiteral(left), ScalarExpr::RetainedLiteral(right)) => {
            retained::same(left, right, control)?
        }
        (ScalarExpr::RetainedLiteral(literal), other)
        | (other, ScalarExpr::RetainedLiteral(literal)) => {
            retained::inline(literal, other, control)?
        }
        (
            ScalarExpr::Collated {
                expr: left,
                ty: left_type,
                collation: left_collation,
            },
            ScalarExpr::Collated {
                expr: right,
                ty: right_type,
                collation: right_collation,
            },
        ) => {
            left_type == right_type
                && left_collation == right_collation
                && exprs_match_with_control(left, right, control)?
        }
        (ScalarExpr::Star, ScalarExpr::Star) => true,
        (ScalarExpr::QualifiedStar(a), ScalarExpr::QualifiedStar(b))
        | (ScalarExpr::Column(a), ScalarExpr::Column(b)) => a == b,
        (
            ScalarExpr::QualifiedColumn {
                qualifier: aq,
                column: ac,
                ..
            },
            ScalarExpr::QualifiedColumn {
                qualifier: bq,
                column: bc,
                ..
            },
        ) => aq == bq && ac == bc,
        (ScalarExpr::Column(c), ScalarExpr::QualifiedColumn { column, .. })
        | (ScalarExpr::QualifiedColumn { column, .. }, ScalarExpr::Column(c)) => c == column,
        (ScalarExpr::Literal(a), ScalarExpr::Literal(b)) => super::literals_equal(a, b),
        (ScalarExpr::TypedLiteral { .. }, ScalarExpr::TypedLiteral { .. }) => {
            typed_literals_match(lhs, rhs)
        }
        (ScalarExpr::Param(a), ScalarExpr::Param(b))
        | (ScalarExpr::Position(a), ScalarExpr::Position(b)) => a == b,
        (ScalarExpr::InternalColumn(a), ScalarExpr::InternalColumn(b)) => a == b,
        (ScalarExpr::Func { .. }, ScalarExpr::Func { .. }) => {
            function_exprs_match(lhs, rhs, control)?
        }
        (
            ScalarExpr::Binary {
                op: ao,
                lhs: al,
                rhs: ar,
            },
            ScalarExpr::Binary {
                op: bo,
                lhs: bl,
                rhs: br,
            },
        ) => {
            ao == bo
                && exprs_match_with_control(al, bl, control)?
                && exprs_match_with_control(ar, br, control)?
        }
        (ScalarExpr::And(a), ScalarExpr::And(b))
        | (ScalarExpr::Or(a), ScalarExpr::Or(b))
        | (ScalarExpr::Array(a), ScalarExpr::Array(b))
        | (ScalarExpr::Row(a), ScalarExpr::Row(b)) => sequence(a, b, control)?,
        (ScalarExpr::Not(a), ScalarExpr::Not(b))
        | (ScalarExpr::UnaryMinus(a), ScalarExpr::UnaryMinus(b)) => {
            exprs_match_with_control(a, b, control)?
        }
        (
            ScalarExpr::CompositeRow {
                items: a,
                binding: ab,
                bound_type: at,
            },
            ScalarExpr::CompositeRow {
                items: b,
                binding: bb,
                bound_type: bt,
            },
        ) => ab == bb && at == bt && sequence(a, b, control)?,
        (
            ScalarExpr::Cast {
                expr: a, ty: at, ..
            },
            ScalarExpr::Cast {
                expr: b, ty: bt, ..
            },
        ) => at == bt && exprs_match_with_control(a, b, control)?,
        _ => false,
    })
}

fn sequence(
    left: &[ScalarExpr],
    right: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        if !exprs_match_with_control(left, right, control)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn function_exprs_match(
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let (
        ScalarExpr::Func {
            order_syntax: aos,
            name: an,
            binding: ab,
            args: aa,
            distinct: ad,
            order_by: ao,
            filter: af,
        },
        ScalarExpr::Func {
            order_syntax: bos,
            name: bn,
            binding: bb,
            args: ba,
            distinct: bd,
            order_by: bo,
            filter: bf,
        },
    ) = (lhs, rhs)
    else {
        return Ok(false);
    };
    if !an.eq_ignore_ascii_case(bn)
        || ab != bb
        || ad != bd
        || aos != bos
        || !sequence(aa, ba, control)?
        || ao.len() != bo.len()
    {
        return Ok(false);
    }
    for (left, right) in ao.iter().zip(bo) {
        if left.descending != right.descending
            || left.nulls != right.nulls
            || !exprs_match_with_control(&left.expr, &right.expr, control)?
        {
            return Ok(false);
        }
    }
    match (af.as_deref(), bf.as_deref()) {
        (None, None) => Ok(true),
        (Some(left), Some(right)) => exprs_match_with_control(left, right, control),
        _ => Ok(false),
    }
}

fn typed_literals_match(lhs: &ScalarExpr, rhs: &ScalarExpr) -> bool {
    let (
        ScalarExpr::TypedLiteral {
            value: av,
            ty: at,
            bound_type: ab,
            parameter_index: ap,
            composite_source: ac,
            collation_oid: ao,
        },
        ScalarExpr::TypedLiteral {
            value: bv,
            ty: bt,
            bound_type: bb,
            parameter_index: bp,
            composite_source: bc,
            collation_oid: bo,
        },
    ) = (lhs, rhs)
    else {
        return false;
    };
    at == bt && ab == bb && ap == bp && ac == bc && ao == bo && super::literals_equal(av, bv)
}

fn retained_value_matches(
    literal: &RetainedScalarLiteral,
    value: &Value,
    other: &ScalarExpr,
) -> bool {
    match (&literal.declaration, other) {
        (None, ScalarExpr::Literal(other)) => super::literals_equal(value, other),
        (
            Some(declaration),
            ScalarExpr::TypedLiteral {
                value: other,
                ty,
                bound_type,
                parameter_index,
                composite_source,
                collation_oid,
            },
        ) => {
            declaration == ty
                && literal.ty == *bound_type
                && parameter_index.is_none()
                && composite_source.is_none()
                && literal.collation_oid == *collation_oid
                && super::literals_equal(value, other)
        }
        _ => false,
    }
}
