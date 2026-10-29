//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed final collation selection admits scratch and copies only surviving identity payloads.

use super::{override_value, Bindings, Collations};
use crate::{
    schema::{
        production::lease::SchemaMemory, ColumnIdentity, ColumnType, InternalColumnRef, RowSchema,
    },
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

type Key<'a> = (Option<&'a str>, &'a str);

pub(in crate::schema) struct Selection<'a, 'c> {
    columns: ProductionMap<'c, usize, u32>,
    aliases: ProductionMap<'c, Key<'a>, u32>,
    internal: ProductionMap<'c, InternalColumnRef, u32>,
    virtual_columns: ProductionMap<'c, Key<'a>, u32>,
}

impl<'a, 'c> Selection<'a, 'c> {
    pub(in crate::schema) fn new(
        input: &'a Collations,
        control: &ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        let mut selected = Self {
            columns: ProductionMap::new(*control),
            aliases: ProductionMap::new(*control),
            internal: ProductionMap::new(*control),
            virtual_columns: ProductionMap::new(*control),
        };
        if let Some(input) = &input.0 {
            for (position, value) in &input.columns {
                selected.columns.insert_copy(*position, *value)?;
            }
            for (identity, value) in &input.aliases {
                selected
                    .aliases
                    .insert_copy((identity.qualifier(), identity.column()), *value)?;
            }
            for (identity, value) in &input.virtual_columns {
                selected
                    .virtual_columns
                    .insert_copy((identity.qualifier(), identity.column()), *value)?;
            }
            for (identity, value) in &input.internal {
                selected.internal.insert_copy(*identity, *value)?;
            }
        }
        Ok(selected)
    }

    pub(in crate::schema) fn alias(
        &mut self,
        identity: Key<'a>,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) -> Result<(), SQLError> {
        if let Some(oid) = override_value(oid, ty) {
            self.aliases.insert_copy(identity, oid)?;
        } else {
            self.aliases.remove(&identity);
        }
        Ok(())
    }

    pub(in crate::schema) fn virtual_column(
        &mut self,
        identity: Key<'a>,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) -> Result<(), SQLError> {
        if let Some(oid) = override_value(oid, ty) {
            self.virtual_columns.insert_copy(identity, oid)?;
        } else {
            self.virtual_columns.remove(&identity);
        }
        Ok(())
    }

    pub(in crate::schema) fn internal(
        &mut self,
        identity: InternalColumnRef,
        oid: Option<u32>,
        ty: Option<&ColumnType>,
    ) -> Result<(), SQLError> {
        if let Some(oid) = override_value(oid, ty) {
            self.internal.insert_copy(identity, oid)?;
        } else {
            self.internal.remove(&identity);
        }
        Ok(())
    }

    pub(in crate::schema) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Collations, SQLError> {
        if self.columns.is_empty()
            && self.aliases.is_empty()
            && self.internal.is_empty()
            && self.virtual_columns.is_empty()
        {
            return Ok(Collations::default());
        }
        let mut memory = control.reserve(size_of::<Bindings>())?;
        let mut output = Bindings::default();
        for (position, oid) in &self.columns {
            super::production::insert(&mut output.columns, *position, *oid, control)?;
        }
        for (identity, oid) in &self.internal {
            super::production::insert(&mut output.internal, *identity, *oid, control)?;
        }
        for (selected, destination) in [
            (&self.aliases, &mut output.aliases),
            (&self.virtual_columns, &mut output.virtual_columns),
        ] {
            for ((qualifier, column), oid) in selected {
                let (identity, lease) =
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)?
                        .into_parts();
                super::production::insert(destination, identity, *oid, control)?;
                memory = control.combine(memory, lease);
            }
        }
        output.memory = SchemaMemory::new(memory);
        let output = Collations(Some(Box::new(output)));
        control.check()?;
        Ok(output)
    }
}

impl Collations {
    pub(in crate::schema) fn joined_with_control(
        left: &RowSchema,
        right: &RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = Selection::new(&left.index.cold.collations, control)?;
        for identity in right.index.cold.aliases.keys() {
            control.check()?;
            selected
                .aliases
                .remove(&(identity.qualifier(), identity.column()));
        }
        for identity in right.index.cold.binding_only.keys() {
            control.check()?;
            selected
                .virtual_columns
                .remove(&(identity.qualifier(), identity.column()));
        }
        if let Some(input) = &right.index.cold.collations.0 {
            for (position, oid) in &input.columns {
                selected.columns.insert_copy(left.len() + position, *oid)?;
            }
            for (identity, oid) in &input.aliases {
                selected
                    .aliases
                    .insert_copy((identity.qualifier(), identity.column()), *oid)?;
            }
            for (identity, oid) in &input.virtual_columns {
                selected
                    .virtual_columns
                    .insert_copy((identity.qualifier(), identity.column()), *oid)?;
            }
            for (identity, oid) in &input.internal {
                selected.internal.insert_copy(*identity, *oid)?;
            }
        }
        selected.finish(control)
    }
}
