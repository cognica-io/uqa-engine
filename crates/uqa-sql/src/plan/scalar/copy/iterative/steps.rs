//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resume constructors in their original field and child order.

use super::{
    parts::{Slot, Vector},
    Frame, Result, ScalarExpr,
};
use crate::ir::{ScalarFrameBound, ScalarOrder, ScalarWindowSpec};

type Next<'s> = Option<(&'s ScalarExpr, Slot)>;

impl<'s> Frame<'s, '_> {
    pub(super) fn step(&mut self) -> Result<Next<'s>> {
        match self.source {
            ScalarExpr::Array(items)
            | ScalarExpr::Row(items)
            | ScalarExpr::And(items)
            | ScalarExpr::Or(items)
            | ScalarExpr::CompositeRow { items, .. } => {
                if self.stage == 0 {
                    self.sequence(items, Vector::List)
                } else {
                    Ok(None)
                }
            }
            ScalarExpr::UnaryMinus(expr)
            | ScalarExpr::Not(expr)
            | ScalarExpr::IsNull { expr, .. }
            | ScalarExpr::Cast { expr, .. }
            | ScalarExpr::Collated { expr, .. }
            | ScalarExpr::InSubquery { expr, .. } => self.boxes(&[expr]),
            ScalarExpr::Binary { lhs, rhs, .. } => self.boxes(&[lhs, rhs]),
            ScalarExpr::Between { expr, low, high } => self.boxes(&[expr, low, high]),
            ScalarExpr::InList { expr, list, .. } => match self.stage {
                0 => self.optional_box(Some(expr), Slot::InList),
                1 => self.sequence(list, Vector::List),
                _ => Ok(None),
            },
            ScalarExpr::Func {
                args,
                order_by,
                filter,
                ..
            } => self.call(args, order_by, filter.as_deref()),
            ScalarExpr::WindowCall {
                args, spec, filter, ..
            } => self.window(args, spec, filter.as_deref()),
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => self.case(base.as_deref(), when, else_branch.as_deref()),
            ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default
            | ScalarExpr::Column(_)
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::RetainedLiteral(_)
            | ScalarExpr::Param(_)
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. } => Ok(None),
        }
    }

    fn sequence(&mut self, items: &'s [ScalarExpr], target: Vector) -> Result<Next<'s>> {
        if self.index == 0 {
            self.parts
                .reserve(target, items.len(), &mut self.lowering)?;
        }
        if let Some(item) = items.get(self.index) {
            self.index += 1;
            return Ok(Some((item, Slot::Vector(target))));
        }
        self.advance();
        Ok(None)
    }

    fn ordering(&mut self, items: &'s [ScalarOrder]) -> Result<Next<'s>> {
        if self.index == 0 {
            self.parts
                .reserve(Vector::Ordering, items.len(), &mut self.lowering)?;
        }
        if let Some(order) = items.get(self.index) {
            self.index += 1;
            return Ok(Some((
                &order.expr,
                Slot::Order {
                    descending: order.descending,
                    nulls: order.nulls,
                },
            )));
        }
        self.advance();
        Ok(None)
    }

    fn optional_box(&mut self, child: Option<&'s ScalarExpr>, slot: Slot) -> Result<Next<'s>> {
        self.advance();
        if let Some(child) = child {
            self.reserve_box()?;
            Ok(Some((child, slot)))
        } else {
            Ok(None)
        }
    }

    fn boxes(&mut self, children: &[&'s ScalarExpr]) -> Result<Next<'s>> {
        match children.get(self.stage) {
            Some(child) => self.optional_box(Some(child), Slot::Box(self.stage)),
            None => Ok(None),
        }
    }

    fn call(
        &mut self,
        args: &'s [ScalarExpr],
        orders: &'s [ScalarOrder],
        filter: Option<&'s ScalarExpr>,
    ) -> Result<Next<'s>> {
        loop {
            let next = match self.stage {
                0 => self.sequence(args, Vector::Arguments)?,
                1 => self.ordering(orders)?,
                2 => self.optional_box(filter, Slot::Filter)?,
                _ => return Ok(None),
            };
            if next.is_some() {
                return Ok(next);
            }
        }
    }

    fn case(
        &mut self,
        base: Option<&'s ScalarExpr>,
        when: &'s [(ScalarExpr, ScalarExpr)],
        otherwise: Option<&'s ScalarExpr>,
    ) -> Result<Next<'s>> {
        loop {
            let next = match self.stage {
                0 => self.optional_box(base, Slot::Base)?,
                1 => {
                    if self.index == 0 {
                        self.parts
                            .reserve(Vector::When, when.len(), &mut self.lowering)?;
                    }
                    if let Some((condition, value)) = when.get(self.index / 2) {
                        let result = if self.index.is_multiple_of(2) {
                            (condition, Slot::Condition)
                        } else {
                            (value, Slot::Result)
                        };
                        self.index += 1;
                        Some(result)
                    } else {
                        self.advance();
                        None
                    }
                }
                2 => self.optional_box(otherwise, Slot::Else)?,
                _ => return Ok(None),
            };
            if next.is_some() {
                return Ok(next);
            }
        }
    }

    fn window(
        &mut self,
        args: &'s [ScalarExpr],
        spec: &'s ScalarWindowSpec,
        filter: Option<&'s ScalarExpr>,
    ) -> Result<Next<'s>> {
        loop {
            let next = match self.stage {
                0 => self.sequence(args, Vector::Arguments)?,
                1 => self.sequence(&spec.partition_by, Vector::Partition)?,
                2 => self.ordering(&spec.order_by)?,
                3 | 4 => {
                    if let Some(frame) = &spec.frame {
                        let start = self.stage == 3;
                        self.bound(if start { &frame.start } else { &frame.end }, start)?
                    } else {
                        self.stage = 5;
                        None
                    }
                }
                5 => self.optional_box(filter, Slot::Filter)?,
                _ => return Ok(None),
            };
            if next.is_some() {
                return Ok(next);
            }
        }
    }

    fn bound(&mut self, bound: &'s ScalarFrameBound, start: bool) -> Result<Next<'s>> {
        let value = match bound {
            ScalarFrameBound::Preceding(expr) => {
                return self.optional_box(
                    Some(expr),
                    Slot::Frame {
                        start,
                        preceding: true,
                    },
                )
            }
            ScalarFrameBound::Following(expr) => {
                return self.optional_box(
                    Some(expr),
                    Slot::Frame {
                        start,
                        preceding: false,
                    },
                )
            }
            ScalarFrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
            ScalarFrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
            ScalarFrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
        };
        self.parts.bound(start, value);
        self.advance();
        Ok(None)
    }
}
