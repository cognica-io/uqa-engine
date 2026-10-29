//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact syntax equality retains the ordinary field order while admitting retained-value observations.

use super::{ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowSpec};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

#[cfg(test)]
mod tests;

impl ScalarExpr {
    /// Compare exactly the same syntax and metadata as `PartialEq`, with cancellation between children and admission for any independent retained-value reads.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive syntax comparison preserves the derived field comparison order"
    )]
    pub fn has_same_syntax_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(match (self, other) {
            (Self::RetainedLiteral(left), Self::RetainedLiteral(right)) => {
                left.has_same_representation_with_control(right, control)?
            }
            (Self::Array(left), Self::Array(right))
            | (Self::Row(left), Self::Row(right))
            | (Self::And(left), Self::And(right))
            | (Self::Or(left), Self::Or(right)) => sequence(left, right, control)?,
            (
                Self::CompositeRow {
                    items: left,
                    binding: lb,
                    bound_type: lt,
                },
                Self::CompositeRow {
                    items: right,
                    binding: rb,
                    bound_type: rt,
                },
            ) => sequence(left, right, control)? && lb == rb && lt == rt,
            (Self::UnaryMinus(left), Self::UnaryMinus(right))
            | (Self::Not(left), Self::Not(right)) => {
                left.has_same_syntax_with_control(right, control)?
            }
            (
                Self::Binary {
                    op: lo,
                    lhs: ll,
                    rhs: lr,
                },
                Self::Binary {
                    op: ro,
                    lhs: rl,
                    rhs: rr,
                },
            ) => {
                lo == ro
                    && ll.has_same_syntax_with_control(rl, control)?
                    && lr.has_same_syntax_with_control(rr, control)?
            }
            (
                Self::Collated {
                    expr: left,
                    ty: lt,
                    collation: lc,
                },
                Self::Collated {
                    expr: right,
                    ty: rt,
                    collation: rc,
                },
            ) => left.has_same_syntax_with_control(right, control)? && lt == rt && lc == rc,
            (
                Self::Cast {
                    implicit: li,
                    expr: left,
                    ty: lt,
                },
                Self::Cast {
                    implicit: ri,
                    expr: right,
                    ty: rt,
                },
            ) => li == ri && left.has_same_syntax_with_control(right, control)? && lt == rt,
            (
                Self::IsNull {
                    expr: left,
                    negated: ln,
                },
                Self::IsNull {
                    expr: right,
                    negated: rn,
                },
            ) => left.has_same_syntax_with_control(right, control)? && ln == rn,
            (
                Self::Between {
                    expr: left,
                    low: ll,
                    high: lh,
                },
                Self::Between {
                    expr: right,
                    low: rl,
                    high: rh,
                },
            ) => {
                left.has_same_syntax_with_control(right, control)?
                    && ll.has_same_syntax_with_control(rl, control)?
                    && lh.has_same_syntax_with_control(rh, control)?
            }
            (
                Self::InList {
                    expr: left,
                    list: ll,
                    negated: ln,
                },
                Self::InList {
                    expr: right,
                    list: rl,
                    negated: rn,
                },
            ) => {
                left.has_same_syntax_with_control(right, control)?
                    && sequence(ll, rl, control)?
                    && ln == rn
            }
            (
                Self::InSubquery {
                    expr: left,
                    subquery: ls,
                    negated: ln,
                },
                Self::InSubquery {
                    expr: right,
                    subquery: rs,
                    negated: rn,
                },
            ) => left.has_same_syntax_with_control(right, control)? && ls == rs && ln == rn,
            (
                Self::Func {
                    name: ln,
                    binding: lb,
                    args: la,
                    distinct: ld,
                    order_by: lo,
                    order_syntax: ls,
                    filter: lf,
                },
                Self::Func {
                    name: rn,
                    binding: rb,
                    args: ra,
                    distinct: rd,
                    order_by: ro,
                    order_syntax: rs,
                    filter: rf,
                },
            ) => {
                ln == rn
                    && lb == rb
                    && sequence(la, ra, control)?
                    && ld == rd
                    && orders(lo, ro, control)?
                    && ls == rs
                    && optional(lf.as_deref(), rf.as_deref(), control)?
            }
            (
                Self::WindowCall {
                    name: ln,
                    args: la,
                    spec: ls,
                    filter: lf,
                    modifiers: lm,
                },
                Self::WindowCall {
                    name: rn,
                    args: ra,
                    spec: rs,
                    filter: rf,
                    modifiers: rm,
                },
            ) => {
                ln == rn
                    && sequence(la, ra, control)?
                    && window(ls, rs, control)?
                    && optional(lf.as_deref(), rf.as_deref(), control)?
                    && lm == rm
            }
            (
                Self::Case {
                    base: lb,
                    when: lw,
                    else_branch: le,
                },
                Self::Case {
                    base: rb,
                    when: rw,
                    else_branch: re,
                },
            ) => {
                if !optional(lb.as_deref(), rb.as_deref(), control)? || lw.len() != rw.len() {
                    return Ok(false);
                }
                for ((lc, lv), (rc, rv)) in lw.iter().zip(rw) {
                    if !lc.has_same_syntax_with_control(rc, control)?
                        || !lv.has_same_syntax_with_control(rv, control)?
                    {
                        return Ok(false);
                    }
                }
                optional(le.as_deref(), re.as_deref(), control)?
            }
            (
                Self::Star
                | Self::QualifiedStar(_)
                | Self::Default
                | Self::Column(_)
                | Self::Position(_)
                | Self::InternalColumn(_)
                | Self::QualifiedColumn { .. }
                | Self::Literal(_)
                | Self::TypedLiteral { .. }
                | Self::Param(_)
                | Self::ScalarSubquery(_)
                | Self::Exists { .. },
                _,
            ) => self == other,
            _ => false,
        })
    }
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
        if !left.has_same_syntax_with_control(right, control)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn optional(
    left: Option<&ScalarExpr>,
    right: Option<&ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    match (left, right) {
        (Some(left), Some(right)) => left.has_same_syntax_with_control(right, control),
        (None, None) => Ok(true),
        _ => Ok(false),
    }
}

fn orders(
    left: &[ScalarOrder],
    right: &[ScalarOrder],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        if !left
            .expr
            .has_same_syntax_with_control(&right.expr, control)?
            || left.descending != right.descending
            || left.nulls != right.nulls
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn window(
    left: &ScalarWindowSpec,
    right: &ScalarWindowSpec,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if left.definition != right.definition
        || !sequence(&left.partition_by, &right.partition_by, control)?
        || !orders(&left.order_by, &right.order_by, control)?
    {
        return Ok(false);
    }
    match (&left.frame, &right.frame) {
        (Some(left), Some(right)) => Ok(left.mode == right.mode
            && bound(&left.start, &right.start, control)?
            && bound(&left.end, &right.end, control)?
            && left.between == right.between
            && left.exclusion == right.exclusion),
        (None, None) => Ok(true),
        _ => Ok(false),
    }
}

fn bound(
    left: &ScalarFrameBound,
    right: &ScalarFrameBound,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    match (left, right) {
        (ScalarFrameBound::Preceding(left), ScalarFrameBound::Preceding(right))
        | (ScalarFrameBound::Following(left), ScalarFrameBound::Following(right)) => {
            left.has_same_syntax_with_control(right, control)
        }
        (
            ScalarFrameBound::UnboundedPreceding
            | ScalarFrameBound::UnboundedFollowing
            | ScalarFrameBound::CurrentRow,
            _,
        ) => Ok(left == right),
        _ => Ok(false),
    }
}
