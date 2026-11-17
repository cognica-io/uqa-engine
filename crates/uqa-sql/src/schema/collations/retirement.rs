//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One original ordered map at a time uses Core's consuming cursor and SQL's field retirement.

use super::{Bindings, Collations, ColumnIdentity, InternalColumnRef};
use crate::{
    ir::ownership::Retirement,
    plan::retirement::{Pending, Retire},
    schema::retirement::fields::{Input, OrderedSlots, Sequence},
};

enum Map {
    Column(Sequence<OrderedSlots<usize, u32>>),
    Identity(Sequence<OrderedSlots<ColumnIdentity, u32>>),
    Internal(Sequence<OrderedSlots<InternalColumnRef, u32>>),
}
impl Retire for Map {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Column(value) => value.retire_step(positions, scalar),
            Self::Identity(value) => value.retire_step(positions, scalar),
            Self::Internal(value) => value.retire_step(positions, scalar),
        }
    }
}

pub(in crate::schema) struct Fields {
    active: Option<Map>,
    position: usize,
    bindings: Box<Bindings>,
}
impl Retire for Fields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        if let Some(active) = &mut self.active {
            if active.retire_step(positions, scalar) {
                self.active = None;
            }
            return false;
        }
        macro_rules! take {
            ($field:ident) => {
                Sequence::new(OrderedSlots(
                    std::mem::take(&mut self.bindings.$field).into_iter(),
                ))
            };
        }
        self.active = Some(match self.position {
            0 => Map::Column(take!(columns)),
            1 => Map::Identity(take!(aliases)),
            2 => Map::Internal(take!(internal)),
            3 => Map::Identity(take!(virtual_columns)),
            _ => return true,
        });
        self.position += 1;
        false
    }
}
impl Collations {
    fn take_retirement(&mut self) -> Option<Pending<Fields>> {
        self.0.take().map(|bindings| {
            Pending::new(Fields {
                active: None,
                position: 0,
                bindings,
            })
        })
    }
}
impl Input for Collations {
    type Retired = Option<Pending<Fields>>;
    fn into_retirement(mut self) -> Self::Retired {
        self.take_retirement()
    }
}
impl Drop for Collations {
    fn drop(&mut self) {
        drop(self.take_retirement());
    }
}
