//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The original index retains its enclosing reservation while one selected field retires.

use super::{
    fields::{Input, Sequence},
    maps::Map,
};
use crate::{
    ast::{ColumnType, InternalColumnRef},
    ir::ownership::Retirement,
    plan::retirement::Retire,
    schema::{
        collations, lateral, record_fields::RecordMetadata, relation_presence, relation_types,
        ColumnIdentity, SchemaColdMetadata, SchemaIndex, ScoreSource,
    },
};

macro_rules! fields {
    ($($name:ident($ty:ty)),* $(,)?) => {
        #[expect(clippy::large_enum_variant, reason = "retirement keeps Core's fixed-height consuming cursor inline and cannot allocate a new box after allowance is exhausted")]
        enum Field { $($name($ty)),* }
        impl Retire for Field {
            fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
                match self { $(Self::$name(value) => value.retire_step(positions, scalar)),* }
            }
        }
    };
}
fields!(
    Names(Vec<String>),
    Identities(Vec<ColumnIdentity>),
    Slots(Vec<usize>),
    NameSlots(Map<Box<str>, usize>),
    IdentitySlots(Map<ColumnIdentity, usize>),
    InternalSlots(Map<InternalColumnRef, usize>),
    NamesSet(Map<Box<str>, ()>),
    IdentitySet(Map<ColumnIdentity, ()>),
    Collations(<collations::Collations as Input>::Retired),
    RowTypes(<relation_types::RelationRowTypes as Input>::Retired),
    Presence(<relation_presence::RelationPresence as Input>::Retired),
    Records(Map<usize, RecordMetadata>),
    Types(Vec<Option<ColumnType>>),
    AliasTypes(Map<ColumnIdentity, Option<ColumnType>>),
    InternalTypes(Map<InternalColumnRef, Option<ColumnType>>),
    Scores(Vec<ScoreSource>),
    SlotSet(Map<usize, ()>),
    OpenSet(Map<Option<Box<str>>, ()>),
    Lateral(Option<<std::sync::Arc<lateral::LateralRestrictions> as Input>::Retired>),
);

pub(super) struct Fields {
    active: Option<Field>,
    position: usize,
    index: SchemaIndex,
}
impl From<SchemaIndex> for Fields {
    fn from(index: SchemaIndex) -> Self {
        Self {
            active: None,
            position: 0,
            index,
        }
    }
}
impl Fields {
    fn next(&mut self) -> Option<Field> {
        let SchemaIndex {
            columns,
            identities,
            slots,
            physical_width: _,
            exact,
            unqualified,
            qualified,
            aliases,
            executor_attributes,
            ambiguous_unqualified,
            ambiguous_qualified,
            cold,
            _memory: _,
        } = &mut self.index;
        let SchemaColdMetadata {
            collations,
            row_types,
            row_presence,
            record_fields,
            columns: types,
            aliases: alias_types,
            executor_attribute_types,
            score_sources,
            wildcard_hidden,
            binding_only,
            open_qualifiers,
            identity_layout: _,
            local_width: _,
            lateral_restrictions,
        } = cold.as_mut();
        macro_rules! map {
            ($field:ident) => {
                Sequence::new(std::mem::take($field).into())
            };
        }
        macro_rules! set {
            ($field:ident) => {
                Sequence::new(std::mem::take($field).into_map().into())
            };
        }
        let field = match self.position {
            0 => Field::Names(std::mem::take(columns).into_vec()),
            1 => Field::Identities(std::mem::take(identities).into_vec()),
            2 => Field::Slots(std::mem::take(slots).into_vec()),
            3 => Field::NameSlots(map!(exact)),
            4 => Field::NameSlots(map!(unqualified)),
            5 => Field::IdentitySlots(map!(qualified)),
            6 => Field::IdentitySlots(map!(aliases)),
            7 => Field::InternalSlots(map!(executor_attributes)),
            8 => Field::NamesSet(set!(ambiguous_unqualified)),
            9 => Field::IdentitySet(set!(ambiguous_qualified)),
            10 => Field::Collations(std::mem::take(collations).into_retirement()),
            11 => Field::RowTypes(std::mem::take(row_types).into_retirement()),
            12 => Field::Presence(std::mem::take(row_presence).into_retirement()),
            13 => Field::Records(map!(record_fields)),
            14 => Field::Types(std::mem::take(types).into_vec()),
            15 => Field::AliasTypes(map!(alias_types)),
            16 => Field::InternalTypes(map!(executor_attribute_types)),
            17 => Field::Scores(std::mem::take(score_sources)),
            18 => Field::SlotSet(set!(wildcard_hidden)),
            19 => Field::AliasTypes(map!(binding_only)),
            20 => Field::OpenSet(set!(open_qualifiers)),
            21 => Field::Lateral(lateral_restrictions.take().map(Input::into_retirement)),
            _ => return None,
        };
        self.position += 1;
        Some(field)
    }
}
impl Retire for Fields {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        if let Some(active) = &mut self.active {
            if active.retire_step(positions, scalar) {
                self.active = None;
            }
            return false;
        }
        self.active = self.next();
        self.active.is_none()
    }
}
