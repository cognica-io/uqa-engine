//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{vector, Lowering, Result, ScalarExpr};
use crate::{
    ast::{ColumnType, FunctionBinding},
    ir::{ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
    plan::scalar::source::Source,
};

pub(super) enum Parts {
    Empty,
    Leaf(ScalarExpr),
    List(Vec<ScalarExpr>),
    Boxes([Option<Box<ScalarExpr>>; 3]),
    InList {
        expr: Option<Box<ScalarExpr>>,
        list: Vec<ScalarExpr>,
    },
    Composite {
        items: Vec<ScalarExpr>,
        bound_type: Option<ColumnType>,
    },
    Call {
        name: String,
        binding: Option<FunctionBinding>,
        args: Vec<ScalarExpr>,
        order_by: Vec<ScalarOrder>,
        filter: Option<Box<ScalarExpr>>,
    },
    Window {
        name: String,
        args: Vec<ScalarExpr>,
        partition: Vec<ScalarExpr>,
        order_by: Vec<ScalarOrder>,
        start: Option<ScalarFrameBound>,
        end: Option<ScalarFrameBound>,
        filter: Option<Box<ScalarExpr>>,
    },
    Case {
        base: Option<Box<ScalarExpr>>,
        when: Vec<(ScalarExpr, ScalarExpr)>,
        condition: Option<ScalarExpr>,
        otherwise: Option<Box<ScalarExpr>>,
    },
}

#[derive(Clone, Copy)]
pub(super) enum Vector {
    List,
    Arguments,
    Partition,
    Ordering,
    When,
}

#[derive(Clone, Copy)]
pub(super) enum Slot {
    Vector(Vector),
    Box(usize),
    InList,
    Filter,
    Order {
        descending: bool,
        nulls: Option<crate::ast::NullsOrder>,
    },
    Frame {
        start: bool,
        preceding: bool,
    },
    Base,
    Condition,
    Result,
    Else,
}

impl Parts {
    pub(super) fn new(source: &ScalarExpr, lowering: &mut Lowering<'_>) -> Result<Self> {
        Ok(match source {
            ScalarExpr::Array(_) | ScalarExpr::Row(_) | ScalarExpr::And(_) | ScalarExpr::Or(_) => {
                Self::List(Vec::new())
            }
            ScalarExpr::UnaryMinus(_)
            | ScalarExpr::Not(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Cast { .. }
            | ScalarExpr::Collated { .. }
            | ScalarExpr::InSubquery { .. }
            | ScalarExpr::Binary { .. }
            | ScalarExpr::Between { .. } => Self::Boxes([None, None, None]),
            ScalarExpr::InList { .. } => Self::InList {
                expr: None,
                list: Vec::new(),
            },
            ScalarExpr::CompositeRow { bound_type, .. } => Self::Composite {
                bound_type: lowering.scalar_type_copy(bound_type.as_ref())?,
                items: Vec::new(),
            },
            ScalarExpr::Func { name, binding, .. } => Self::Call {
                name: lowering.copy_text(name)?,
                binding: binding
                    .as_ref()
                    .map(|binding| lowering.binding(Source::Borrowed(binding)))
                    .transpose()?,
                args: Vec::new(),
                order_by: Vec::new(),
                filter: None,
            },
            ScalarExpr::WindowCall { name, .. } => Self::Window {
                name: lowering.copy_text(name)?,
                args: Vec::new(),
                partition: Vec::new(),
                order_by: Vec::new(),
                start: None,
                end: None,
                filter: None,
            },
            ScalarExpr::Case { .. } => Self::Case {
                base: None,
                when: Vec::new(),
                condition: None,
                otherwise: None,
            },
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
            | ScalarExpr::Exists { .. } => Self::Leaf(lowering.scalar_leaf_copy(source)?),
        })
    }

    pub(super) fn reserve(
        &mut self,
        target: Vector,
        count: usize,
        lowering: &mut Lowering<'_>,
    ) -> Result<()> {
        match (target, self) {
            (
                Vector::List,
                Self::List(items)
                | Self::InList { list: items, .. }
                | Self::Composite { items, .. },
            ) => *items = vector(count, lowering)?,
            (Vector::Arguments, Self::Call { args, .. } | Self::Window { args, .. }) => {
                *args = vector(count, lowering)?;
            }
            (Vector::Partition, Self::Window { partition, .. }) => {
                *partition = vector(count, lowering)?;
            }
            (Vector::Ordering, Self::Call { order_by, .. } | Self::Window { order_by, .. }) => {
                *order_by = vector(count, lowering)?;
            }
            (Vector::When, Self::Case { when, .. }) => *when = vector(count, lowering)?,
            _ => unreachable!("scalar destination container"),
        }
        Ok(())
    }

    pub(super) fn bound(&mut self, is_start: bool, bound: ScalarFrameBound) {
        let Self::Window { start, end, .. } = self else {
            unreachable!("window frame")
        };
        *if is_start { start } else { end } = Some(bound);
    }

    pub(super) fn attach(&mut self, slot: Slot, child: ScalarExpr) {
        match (slot, self) {
            (
                Slot::Vector(Vector::List),
                Self::List(items)
                | Self::InList { list: items, .. }
                | Self::Composite { items, .. },
            ) => items.push(child),
            (
                Slot::Vector(Vector::Arguments),
                Self::Call { args, .. } | Self::Window { args, .. },
            ) => args.push(child),
            (Slot::Vector(Vector::Partition), Self::Window { partition, .. }) => {
                partition.push(child);
            }
            (Slot::Box(index), Self::Boxes(boxes)) => boxes[index] = Some(Box::new(child)),
            (Slot::InList, Self::InList { expr, .. }) => *expr = Some(Box::new(child)),
            (Slot::Filter, Self::Call { filter, .. } | Self::Window { filter, .. }) => {
                *filter = Some(Box::new(child));
            }
            (
                Slot::Order { descending, nulls },
                Self::Call { order_by, .. } | Self::Window { order_by, .. },
            ) => order_by.push(ScalarOrder {
                expr: child,
                descending,
                nulls,
            }),
            (Slot::Frame { start, preceding }, parts) => parts.bound(
                start,
                if preceding {
                    ScalarFrameBound::Preceding(Box::new(child))
                } else {
                    ScalarFrameBound::Following(Box::new(child))
                },
            ),
            (Slot::Base, Self::Case { base, .. }) => *base = Some(Box::new(child)),
            (Slot::Condition, Self::Case { condition, .. }) => *condition = Some(child),
            (
                Slot::Result,
                Self::Case {
                    when, condition, ..
                },
            ) => when.push((condition.take().expect("completed CASE condition"), child)),
            (Slot::Else, Self::Case { otherwise, .. }) => *otherwise = Some(Box::new(child)),
            _ => unreachable!("scalar destination slot"),
        }
    }

    pub(super) fn finish(
        self,
        source: &ScalarExpr,
        lowering: &mut Lowering<'_>,
    ) -> Result<ScalarExpr> {
        Ok(match (source, self) {
            (_, Self::Leaf(expression)) => expression,
            (ScalarExpr::Array(_), Self::List(items)) => ScalarExpr::Array(items),
            (ScalarExpr::Row(_), Self::List(items)) => ScalarExpr::Row(items),
            (ScalarExpr::And(_), Self::List(items)) => ScalarExpr::And(items),
            (ScalarExpr::Or(_), Self::List(items)) => ScalarExpr::Or(items),
            (ScalarExpr::CompositeRow { binding, .. }, Self::Composite { items, bound_type }) => {
                ScalarExpr::CompositeRow {
                    bound_type,
                    items,
                    binding: lowering.composite_binding(Source::Borrowed(binding))?,
                }
            }
            (ScalarExpr::InList { negated, .. }, Self::InList { expr, list }) => {
                ScalarExpr::InList {
                    expr: expr.expect("list input"),
                    list,
                    negated: *negated,
                }
            }
            (
                ScalarExpr::Func {
                    distinct,
                    order_syntax,
                    ..
                },
                Self::Call {
                    name,
                    binding,
                    args,
                    order_by,
                    filter,
                },
            ) => ScalarExpr::Func {
                name,
                binding,
                args,
                distinct: *distinct,
                order_by,
                order_syntax: *order_syntax,
                filter,
            },
            (
                ScalarExpr::WindowCall {
                    spec, modifiers, ..
                },
                Self::Window {
                    name,
                    args,
                    partition,
                    order_by,
                    start,
                    end,
                    filter,
                },
            ) => ScalarExpr::WindowCall {
                name,
                args,
                spec: ScalarWindowSpec {
                    definition: spec.definition,
                    partition_by: partition,
                    order_by,
                    frame: spec.frame.as_ref().map(|frame| ScalarWindowFrame {
                        mode: frame.mode,
                        start: start.expect("window start"),
                        end: end.expect("window end"),
                        between: frame.between,
                        exclusion: frame.exclusion,
                    }),
                },
                filter,
                modifiers: *modifiers,
            },
            (
                ScalarExpr::Case { .. },
                Self::Case {
                    base,
                    when,
                    otherwise,
                    condition: None,
                },
            ) => ScalarExpr::Case {
                base,
                when,
                else_branch: otherwise,
            },
            (source, Self::Boxes(boxes)) => finish_boxes(source, boxes, lowering)?,
            _ => unreachable!("completed scalar constructor"),
        })
    }
}

fn finish_boxes(
    source: &ScalarExpr,
    [first, second, third]: [Option<Box<ScalarExpr>>; 3],
    lowering: &mut Lowering<'_>,
) -> Result<ScalarExpr> {
    let expr = first.expect("first scalar child");
    Ok(match source {
        ScalarExpr::Binary { op, .. } => ScalarExpr::Binary {
            op: *op,
            lhs: expr,
            rhs: second.expect("right scalar child"),
        },
        ScalarExpr::Between { .. } => ScalarExpr::Between {
            expr,
            low: second.expect("lower bound"),
            high: third.expect("upper bound"),
        },
        ScalarExpr::UnaryMinus(_) => ScalarExpr::UnaryMinus(expr),
        ScalarExpr::Not(_) => ScalarExpr::Not(expr),
        ScalarExpr::IsNull { negated, .. } => ScalarExpr::IsNull {
            expr,
            negated: *negated,
        },
        ScalarExpr::Cast { implicit, ty, .. } => ScalarExpr::Cast {
            expr,
            ty: lowering.copy_text(ty)?,
            implicit: *implicit,
        },
        ScalarExpr::Collated { ty, collation, .. } => ScalarExpr::Collated {
            expr,
            ty: lowering
                .scalar_type_copy(Some(ty))?
                .expect("retained result type"),
            collation: *collation,
        },
        ScalarExpr::InSubquery {
            subquery, negated, ..
        } => ScalarExpr::InSubquery {
            expr,
            subquery: *subquery,
            negated: *negated,
        },
        _ => unreachable!("boxed scalar constructor"),
    })
}
