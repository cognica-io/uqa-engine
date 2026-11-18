//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Parts, Retire};
use crate::plan::retirement::scalar_fields;

impl Retire for Parts {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        match self {
            Self::Empty => true,
            Self::Leaf(value) => value.retire_step(positions, scalar),
            Self::List(items) | Self::Composite { items, .. } => {
                items.retire_step(positions, scalar)
            }
            Self::Boxes(boxes) => boxes.retire_step(positions, scalar),
            Self::InList { expr, list } => scalar_fields!(positions, scalar; expr, list),
            Self::Call {
                args,
                order_by,
                filter,
                ..
            } => scalar_fields!(positions, scalar; args, order_by, filter),
            Self::Window {
                args,
                partition,
                order_by,
                start,
                end,
                filter,
                ..
            } => scalar_fields!(positions, scalar; args, partition, order_by, start, end, filter),
            Self::Case {
                base,
                when,
                condition,
                otherwise,
            } => scalar_fields!(positions, scalar; base, when, condition, otherwise),
        }
    }
}
