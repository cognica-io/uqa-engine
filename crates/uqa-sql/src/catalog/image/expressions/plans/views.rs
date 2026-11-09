//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored views stream native metadata, authority, evaluated rows and original query syntax.

use super::{fields, variant, DefinitionImageReader, Frame, ImageFrame, Node, Part, SQLError};
use crate::{
    ast::RelationPersistence,
    catalog::{
        relation_oids::RelationCatalogOids,
        security::{table_binding::BoundTableAclEntry, BoundTableSecurity},
        stored_view::{CapturedViewDefinitions, StoredView, StoredViewDefinition, ViewDefinitions},
        view::StoredViewKind,
    },
};
use uqa_core::{
    catalog_acl::RelationAclRevisions,
    catalog_role::RoleIdentity,
    memory::{BudgetedMapIter, MemoryBudget, MemoryReservation, OwnedMap},
    CancellationToken, Value,
};

impl CapturedViewDefinitions {
    /// Borrow the durable generation without copying definitions, evaluating queries or materializing rows.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        let mut reader = DefinitionImageReader::start(
            ImageFrame::NextView,
            *b"UQAVIW01",
            self.len(),
            memory,
            cancellation,
        )?;
        reader.views = Some(self.iter());
        Ok(reader)
    }
}

impl ViewDefinitions {
    /// Reconstruct an unpublished immutable view catalog, preserving original bindings and evaluated cells. Cross-catalog validation remains with the enclosing restore owner.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        Self::read_image_with_mutation_budget(reader, memory, memory, cancellation)
    }

    /// Construct original definitions under the input allowance and fund subsequent native mutations through the destination owner. Captures and untouched definitions retain their original reservations without keeping input cancellation.
    pub fn read_image_with_mutation_budget(
        reader: &mut dyn std::io::Read,
        input: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        Ok(
            super::super::super::input::views(reader, input, cancellation)?
                .into_mutation_budget(mutations),
        )
    }
}

image_struct!(StoredView {
    security,
    definition
});
image_struct!(StoredViewDefinition {
    object_id,
    output_columns,
    persistence,
    options,
    kind,
    materialized_rows,
    materialized_column_types,
    materialized_column_collations,
    populated,
    catalog_oids,
    row_type_array_name,
    query,
});
image_struct!(BoundTableSecurity {
    role_owner,
    acl,
    column_acls,
    acl_revisions
});
image_struct!(RelationAclRevisions { relation, columns });
image_struct!(RelationCatalogOids {
    relation,
    row_type,
    array_type,
    rule
});
image_enum!(RelationPersistence { 0 => Permanent, 1 => Unlogged, 2 => Temporary });
image_enum!(StoredViewKind { 0 => View, 1 => Materialized });

impl Node for (String, String) {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(super::fields(position, &[&self.0, &self.1]))
    }
}
impl Node for RoleIdentity {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(match position {
            0 => Some(Part::U64(self.oid as u64)),
            1 => Some(Part::Bytes(&self.object_id)),
            _ => None,
        })
    }
}
impl Node for BoundTableAclEntry {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok((position == 0).then_some(Part::ViewAcl(self)))
    }
}

pub(in super::super) enum Map<'a> {
    Cells(&'a OwnedMap<String, Value>),
    Acls(&'a OwnedMap<String, Vec<BoundTableAclEntry>>),
    Revisions(&'a OwnedMap<String, [u8; 16]>),
}
pub(in super::super) enum Cursor<'a> {
    Cells(BudgetedMapIter<'a, String, Value>),
    Acls(BudgetedMapIter<'a, String, Vec<BoundTableAclEntry>>),
    Revisions(BudgetedMapIter<'a, String, [u8; 16]>),
}

macro_rules! map_node {
    ($ty:ty, $variant:ident) => {
        impl Node for OwnedMap<String, $ty> {
            fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
                Ok(match position {
                    0 => Some(Part::Count(self.len())),
                    1 => Some(Part::ViewMap(Map::$variant(self))),
                    _ => None,
                })
            }
        }
    };
}
map_node!(Value, Cells);
map_node!(Vec<BoundTableAclEntry>, Acls);
map_node!([u8; 16], Revisions);

impl<'a> DefinitionImageReader<'a> {
    pub(in super::super) fn next_view(&mut self) -> Result<(), SQLError> {
        if let Some((key, value)) = self.views.as_mut().and_then(Iterator::next) {
            self.fields.text(&key.schema);
            self.fields.text(&key.name);
            self.push(ImageFrame::NextView)?;
            self.push(ImageFrame::Plan(Frame::Node(value, 0)))?;
        }
        Ok(())
    }

    pub(super) fn start_view_map(&mut self, map: Map<'a>) -> Result<(), SQLError> {
        let memory = self.memory.reserve(size_of::<Cursor<'_>>())?;
        let cursor = match map {
            Map::Cells(values) => Cursor::Cells(values.iter()),
            Map::Acls(values) => Cursor::Acls(values.iter()),
            Map::Revisions(values) => Cursor::Revisions(values.iter()),
        };
        self.push(ImageFrame::Plan(Frame::ViewMap(Box::new(cursor), memory)))
    }

    pub(super) fn view_map(
        &mut self,
        mut cursor: Box<Cursor<'a>>,
        memory: MemoryReservation,
    ) -> Result<(), SQLError> {
        let next = match cursor.as_mut() {
            Cursor::Cells(values) => values.next().map(|(key, value)| (key, Part::Value(value))),
            Cursor::Acls(values) => values.next().map(|(key, value)| (key, Part::Node(value))),
            Cursor::Revisions(values) => {
                values.next().map(|(key, value)| (key, Part::Bytes(value)))
            }
        };
        if let Some((key, value)) = next {
            self.fields.text(key);
            self.push(ImageFrame::Plan(Frame::ViewMap(cursor, memory)))?;
            self.plan_field(value)?;
        }
        Ok(())
    }
}
