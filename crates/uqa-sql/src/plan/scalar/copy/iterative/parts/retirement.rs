//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Parts, Retire};

impl Retire for Parts {
    fn retire(&mut self) {
        match self {
            Self::Empty => {}
            Self::Leaf(value) => value.retire(),
            Self::List(items) | Self::Composite { items, .. } => items.retire(),
            Self::Boxes(boxes) => {
                for value in boxes {
                    value.retire();
                }
            }
            Self::InList { expr, list } => {
                expr.retire();
                list.retire();
            }
            Self::Call {
                args,
                order_by,
                filter,
                ..
            } => {
                args.retire();
                order_by.retire();
                filter.retire();
            }
            Self::Window {
                args,
                partition,
                order_by,
                start,
                end,
                filter,
                ..
            } => {
                args.retire();
                partition.retire();
                order_by.retire();
                start.retire();
                end.retire();
                filter.retire();
            }
            Self::Case {
                base,
                when,
                condition,
                otherwise,
            } => {
                base.retire();
                for (condition, result) in when {
                    condition.retire();
                    result.retire();
                }
                condition.retire();
                otherwise.retire();
            }
        }
    }
}
