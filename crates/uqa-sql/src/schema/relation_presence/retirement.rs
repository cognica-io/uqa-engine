//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, InternalColumnRef, PresenceBindings, RelationPresence};
use crate::{
    ir::ownership::Retirement,
    plan::retirement::{Pending, Retire},
    schema::{
        production::lease::SchemaMemory,
        retirement::{
            fields::{Input, Sequence},
            maps::Map,
        },
    },
};

pub(in crate::schema) struct Fields {
    bindings: Map<Box<str>, InternalColumnRef>,
    _memory: SchemaMemory,
}
impl From<PresenceBindings> for Fields {
    fn from(bindings: PresenceBindings) -> Self {
        let PresenceBindings {
            bindings,
            _memory: memory,
        } = bindings;
        Self {
            bindings: Sequence::new(bindings.into()),
            _memory: memory,
        }
    }
}
impl Retire for Fields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.bindings.retire_step(positions, scalar)
    }
}
impl Input for RelationPresence {
    type Retired = Option<Pending<Fields>>;
    fn into_retirement(mut self) -> Self::Retired {
        self.0
            .take()
            .and_then(Arc::into_inner)
            .map(|rows| Pending::new(rows.into()))
    }
}
impl Drop for RelationPresence {
    fn drop(&mut self) {
        drop(
            self.0
                .take()
                .and_then(Arc::into_inner)
                .map(|rows| Pending::new(Fields::from(rows))),
        );
    }
}
