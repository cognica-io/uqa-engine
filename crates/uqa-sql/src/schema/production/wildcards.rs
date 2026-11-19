//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    catalog::work::{inline, retire_entries, ReferenceWork, Work},
    schema::{ColumnIdentity, RowSchema, NULL_SLOT},
    ColumnType, SQLError,
};
use std::cmp::Ordering;
use uqa_core::{
    memory::{MapKeyInput, ProductionControl, ProductionMap, ProductionVec},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Clone, Copy)]
struct Entry<'a> {
    identity: &'a ColumnIdentity,
    logical: Option<usize>,
    slot: usize,
    ty: Option<&'a ColumnType>,
    ordinal: usize,
}

impl<'a, W: Work + Send> SortProgress<Entry<'a>, SQLError> for ReferenceWork<'_, '_, W> {
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        ReferenceWork::checkpoint(self).await
    }

    async fn compare(&mut self, left: &Entry<'a>, right: &Entry<'a>) -> Result<Ordering, SQLError> {
        Ok((left.slot, left.ordinal).cmp(&(right.slot, right.ordinal)))
    }
}

impl<'q, 's, W: Work + Send> MapKeyInput<(&'q ColumnIdentity, usize), (&'s ColumnIdentity, usize)>
    for ReferenceWork<'_, '_, W>
{
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        ReferenceWork::checkpoint(self).await
    }

    async fn compare(
        &mut self,
        left: &(&'q ColumnIdentity, usize),
        right: &(&'s ColumnIdentity, usize),
    ) -> Result<Ordering, SQLError> {
        let identity = MapKeyInput::compare(self, left.0, right.0).await?;
        Ok(identity.then_with(|| left.1.cmp(&right.1)))
    }
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
        inline(self.visit_qualified_star_entries(
            qualifier,
            &mut ReferenceWork {
                control,
                work: &mut (),
            },
            |entry| visit(entry.identity.column(), entry.logical, entry.slot, entry.ty),
        ))
    }

    pub(crate) async fn visit_qualified_star_metadata<'a, W: Work + Send>(
        &'a self,
        qualifier: &str,
        input: &mut ReferenceWork<'_, '_, W>,
        mut visit: impl FnMut(
            &'a str,
            Option<usize>,
            Option<&'a ColumnType>,
            Option<u32>,
            Option<&'a crate::schema::RecordMetadata>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.visit_qualified_star_entries(qualifier, input, |entry| {
            let collation = entry.logical.map_or_else(
                || self.qualified_identity_collation(entry.identity),
                |logical| self.column_collation(logical),
            );
            visit(
                entry.identity.column(),
                entry.logical,
                entry.ty,
                collation,
                self.physical_record_metadata(entry.slot),
            )
        })
        .await
    }

    async fn visit_qualified_star_entries<'a, W: Work + Send>(
        &'a self,
        qualifier: &str,
        input: &mut ReferenceWork<'_, '_, W>,
        mut visit: impl FnMut(Entry<'a>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let control = input.control;
        control.check()?;
        let mut entries = ProductionVec::new(*control);
        let mut visible = ProductionMap::new(*control);
        for (logical, identity) in self.identities().iter().enumerate() {
            input.checkpoint().await?;
            if identity.qualifier() == Some(qualifier) {
                let slot = self.slot(logical).unwrap_or(NULL_SLOT);
                visible
                    .insert_copy_from((identity, slot), (), input)
                    .await?;
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
            input.checkpoint().await?;
            if identity.qualifier() == Some(qualifier)
                && visible.get_from(&(identity, *slot), input).await?.is_none()
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
        sort_by_with_progress(entries.as_mut_slice(), input).await?;
        for entry in &*entries {
            input.checkpoint().await?;
            visit(*entry)?;
        }
        retire_entries(visible.into_iter(), control, input.work).await
    }
}

#[cfg(test)]
mod tests;
