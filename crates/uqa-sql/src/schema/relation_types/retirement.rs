//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, ColumnType, RelationRowTypes, RelationRows};
use crate::{
    ir::ownership::Retirement,
    plan::retirement::{scalar_fields, Pending, Retire},
    schema::{
        production::lease::SchemaMemory,
        retirement::{
            fields::{Input, Sequence},
            maps::Map,
        },
    },
};

pub(in crate::schema) struct Fields {
    types: Map<Box<str>, Option<ColumnType>>,
    source: Option<Box<str>>,
    _memory: SchemaMemory,
}
impl From<RelationRows> for Fields {
    fn from(rows: RelationRows) -> Self {
        let RelationRows {
            types,
            source,
            _memory: memory,
        } = rows;
        Self {
            types: Sequence::new(types.into()),
            source,
            _memory: memory,
        }
    }
}
impl Retire for Fields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        scalar_fields!(positions, scalar; self.types, self.source)
    }
}
impl Input for RelationRowTypes {
    type Retired = Option<Pending<Fields>>;
    fn into_retirement(mut self) -> Self::Retired {
        self.0
            .take()
            .and_then(Arc::into_inner)
            .map(|rows| Pending::new(rows.into()))
    }
}
impl Drop for RelationRowTypes {
    fn drop(&mut self) {
        drop(
            self.0
                .take()
                .and_then(Arc::into_inner)
                .map(|rows| Pending::new(Fields::from(rows))),
        );
    }
}
