//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    schema::{ColumnIdentity, RowSchema, NULL_SLOT},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

#[derive(Clone, Copy)]
struct Entry<'a> {
    identity: &'a ColumnIdentity,
    logical: Option<usize>,
    slot: usize,
    ty: Option<&'a ColumnType>,
    ordinal: usize,
}

impl RowSchema {
    /// Visit a relation's wildcard positions in the same physical order as `qualified_star_position_layout`, retaining visible logical positions and hidden aliases without copying their names or types.
    pub fn visit_qualified_star_positions<'a>(
        &'a self,
        qualifier: &str,
        control: &ProductionControl<'_>,
        mut visit: impl FnMut(
            &'a str,
            Option<usize>,
            usize,
            Option<&'a ColumnType>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let mut entries = ProductionVec::new(*control);
        let mut visible = ProductionMap::new(*control);
        for (logical, identity) in self.identities().iter().enumerate() {
            control.check()?;
            if identity.qualifier() == Some(qualifier) {
                let slot = self.slot(logical).unwrap_or(NULL_SLOT);
                visible.insert_copy((identity, slot), ())?;
                entries.push_copy(Entry {
                    identity,
                    logical: Some(logical),
                    slot,
                    ty: self.column_type(logical),
                    ordinal: entries.len(),
                })?;
            }
        }
        for (identity, slot) in &self.index.aliases {
            control.check()?;
            if identity.qualifier() == Some(qualifier) && visible.get(&(identity, *slot)).is_none()
            {
                entries.push_copy(Entry {
                    identity,
                    logical: None,
                    slot: *slot,
                    ty: self
                        .index
                        .cold
                        .aliases
                        .get(identity)
                        .and_then(Option::as_ref),
                    ordinal: entries.len(),
                })?;
            }
        }
        uqa_core::ordering::sort_by_with_control(
            entries.as_mut_slice(),
            &mut || control.check().map_err(SQLError::from),
            |left, right, _| Ok((left.slot, left.ordinal).cmp(&(right.slot, right.ordinal))),
        )?;
        for entry in &*entries {
            control.check()?;
            visit(entry.identity.column(), entry.logical, entry.slot, entry.ty)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
