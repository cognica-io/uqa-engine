//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed scalar children share field selection across observation, rewriting and constructor ownership.

use super::{ScalarExpr, ScalarFrameBound, ScalarOrder};

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Complete,
    Construction,
    /// Aggregate projection slots bind window specifications before arguments and FILTER.
    AggregateDependencies,
    Observation {
        call_order: bool,
    },
}

fn take_option<T>(value: &mut Option<T>, index: &mut usize) -> Option<T> {
    let value = value.take()?;
    if *index == 0 {
        Some(value)
    } else {
        *index -= 1;
        None
    }
}

fn take_iterator<I: ExactSizeIterator>(values: &mut I, index: &mut usize) -> Option<I::Item> {
    let count = values.len();
    let value = values.nth(*index);
    if value.is_none() {
        *index -= count;
    }
    value
}

// Shared and exclusive borrows have the same field order. Slice skipping remains constant-time, including constructor lookups in wide calls and CASE expressions.
macro_rules! children {
    ($name:ident, $method:ident, $slice:ident, $iter:ident, $deref:ident, $reference:ident, [$($mutable:tt)*]) => {
        pub(crate) enum $name<'a> {
            Fixed([Option<&'a $($mutable)* ScalarExpr>; 3]),
            Sequence(std::slice::$slice<'a, ScalarExpr>),
            List {
                head: Option<&'a $($mutable)* ScalarExpr>,
                rest: std::slice::$slice<'a, ScalarExpr>,
            },
            Call {
                args: std::slice::$slice<'a, ScalarExpr>,
                order: std::slice::$slice<'a, ScalarOrder>,
                filter: Option<&'a $($mutable)* ScalarExpr>,
            },
            Window {
                args: std::slice::$slice<'a, ScalarExpr>,
                filter: Option<&'a $($mutable)* ScalarExpr>,
                partition: std::slice::$slice<'a, ScalarExpr>,
                order: std::slice::$slice<'a, ScalarOrder>,
                bounds: [Option<&'a $($mutable)* ScalarExpr>; 2],
                scope: Scope,
            },
            Case {
                base: Option<&'a $($mutable)* ScalarExpr>,
                pairs: std::slice::$slice<'a, (ScalarExpr, ScalarExpr)>,
                pending: Option<&'a $($mutable)* ScalarExpr>,
                otherwise: Option<&'a $($mutable)* ScalarExpr>,
            },
        }

        impl<'a> Iterator for $name<'a> {
            type Item = &'a $($mutable)* ScalarExpr;

            fn next(&mut self) -> Option<Self::Item> {
                self.nth(0)
            }

            fn nth(&mut self, mut index: usize) -> Option<Self::Item> {
                match self {
                    Self::Fixed(items) => items.iter_mut().find_map(|item| take_option(item, &mut index)),
                    Self::Sequence(items) => items.nth(index),
                    Self::List { head, rest } => take_option(head, &mut index)
                        .or_else(|| rest.nth(index)),
                    Self::Call { args, order, filter } => take_iterator(args, &mut index)
                        .or_else(|| take_iterator(order, &mut index).map(|order| & $($mutable)* order.expr))
                        .or_else(|| take_option(filter, &mut index)),
                    Self::Window { args, filter, partition, order, bounds, scope } => {
                        let spec_first = matches!(scope, Scope::AggregateDependencies);
                        let filter_first = matches!(scope, Scope::Complete);
                        if !spec_first {
                            if let Some(value) = take_iterator(args, &mut index) { return Some(value); }
                        }
                        (if filter_first { take_option(filter, &mut index) } else { None })
                            .or_else(|| take_iterator(partition, &mut index))
                            .or_else(|| take_iterator(order, &mut index).map(|order| & $($mutable)* order.expr))
                            .or_else(|| bounds.iter_mut().find_map(|bound| take_option(bound, &mut index)))
                            .or_else(|| if spec_first { take_iterator(args, &mut index) } else { None })
                            .or_else(|| if !filter_first { take_option(filter, &mut index) } else { None })
                    }
                    Self::Case { base, pairs, pending, otherwise } => {
                        if let Some(value) = take_option(base, &mut index)
                            .or_else(|| take_option(pending, &mut index)) {
                            return Some(value);
                        }
                        let count = pairs.len() * 2;
                        if index < count {
                            let (condition, result) = pairs.nth(index / 2)?;
                            if index.is_multiple_of(2) {
                                *pending = Some(result);
                                Some(condition)
                            } else {
                                Some(result)
                            }
                        } else {
                            pairs.nth(pairs.len());
                            index -= count;
                            take_option(otherwise, &mut index)
                        }
                    }
                }
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                let count = self.len();
                (count, Some(count))
            }
        }

        impl ExactSizeIterator for $name<'_> {
            fn len(&self) -> usize {
                match self {
                    Self::Fixed(items) => items.iter().filter(|item| item.is_some()).count(),
                    Self::Sequence(items) => items.len(),
                    Self::List { head, rest } => usize::from(head.is_some()) + rest.len(),
                    Self::Call { args, order, filter } => args.len() + order.len() + usize::from(filter.is_some()),
                    Self::Window { args, filter, partition, order, bounds, .. } => args.len()
                        + usize::from(filter.is_some()) + partition.len() + order.len()
                        + bounds.iter().filter(|bound| bound.is_some()).count(),
                    Self::Case { base, pairs, pending, otherwise } => usize::from(base.is_some())
                        + pairs.len() * 2 + usize::from(pending.is_some()) + usize::from(otherwise.is_some()),
                }
            }
        }

        impl ScalarExpr {
            pub(crate) fn $method(& $($mutable)* self, scope: Scope) -> $name<'_> {
                match self {
                    Self::Array(items) | Self::Row(items) | Self::CompositeRow { items, .. }
                    | Self::And(items) | Self::Or(items) => $name::Sequence(items.$iter()),
                    Self::Not(expr) | Self::UnaryMinus(expr) | Self::Cast { expr, .. }
                    | Self::Collated { expr, .. } | Self::IsNull { expr, .. }
                    | Self::InSubquery { expr, .. } => $name::Fixed([Some(expr.$reference()), None, None]),
                    Self::Binary { lhs, rhs, .. } => $name::Fixed([Some(lhs.$reference()), Some(rhs.$reference()), None]),
                    Self::Between { expr, low, high } => $name::Fixed([Some(expr.$reference()), Some(low.$reference()), Some(high.$reference())]),
                    Self::InList { expr, list, .. } => $name::List { head: Some(expr.$reference()), rest: list.$iter() },
                    Self::Func { args, order_by, filter, .. } => $name::Call {
                        args: args.$iter(),
                        order: if matches!(scope, Scope::Observation { call_order: false }) { [].$iter() } else { order_by.$iter() },
                        filter: filter.$deref(),
                    },
                    Self::WindowCall { args, spec, filter, .. } => {
                        if matches!(scope, Scope::Observation { .. }) {
                            return $name::Fixed([None, None, None]);
                        }
                        let mut bounds = [None, None];
                        if let Some(frame) = spec.frame.$reference() {
                            for (slot, bound) in bounds.iter_mut().zip([& $($mutable)* frame.start, & $($mutable)* frame.end]) {
                                if let ScalarFrameBound::Preceding(value) | ScalarFrameBound::Following(value) = bound {
                                    *slot = Some(value.$reference());
                                }
                            }
                        }
                        $name::Window {
                            args: args.$iter(), filter: filter.$deref(), partition: spec.partition_by.$iter(),
                            order: spec.order_by.$iter(), bounds, scope,
                        }
                    }
                    Self::Case { base, when, else_branch } => $name::Case {
                        base: base.$deref(), pairs: when.$iter(), pending: None, otherwise: else_branch.$deref(),
                    },
                    Self::Default | Self::Star | Self::QualifiedStar(_) | Self::Column(_)
                    | Self::Position(_) | Self::InternalColumn(_) | Self::QualifiedColumn { .. }
                    | Self::Literal(_) | Self::TypedLiteral { .. } | Self::RetainedLiteral(_)
                    | Self::Param(_) | Self::ScalarSubquery(_) | Self::Exists { .. } => $name::Fixed([None, None, None]),
                }
            }
        }
    };
}

children!(Children, children, Iter, iter, as_deref, as_ref, []);
children!(ChildrenMut, children_mut, IterMut, iter_mut, as_deref_mut, as_mut, [mut]);

#[cfg(test)]
mod tests;
