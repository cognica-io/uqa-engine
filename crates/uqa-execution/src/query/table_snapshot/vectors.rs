//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical vector copies retain evaluated canonical sources by column incarnation and complete row selection.

use super::{snapshot_error, DocumentChanges, RowLayout, SnapshotSchema, VectorDimensions};
use std::sync::Arc;
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{
    read_control::StorageReadControl, vector_index::RetainedVectorIndexBuilder, VectorIndex,
};

pub(super) enum SnapshotVector {
    Reconstructed(RetainedVectorIndexBuilder),
    Physical(Arc<dyn VectorIndex>),
}

pub(super) struct VectorSources<'a> {
    pub indexes: &'a dyn VectorDimensions,
    pub columns: &'a [ColumnDef],
    pub layout: &'a RowLayout,
    pub changes: &'a DocumentChanges,
    pub all_rows_changed: bool,
}

impl VectorSources<'_> {
    pub(super) fn retain(
        &self,
        field: &str,
        dimensions: u32,
        schema: &SnapshotSchema<'_>,
        control: &StorageReadControl,
    ) -> Result<Option<Arc<dyn VectorIndex>>, SQLError> {
        let column = schema.columns.iter().find(|column| column.name == field);
        if self.all_rows_changed {
            let retained = (|| {
                let Some(selected) = schema.vector_dimensions.index(field) else {
                    return Ok(None);
                };
                let Some(changes) = self.changes.diskann_read_changes(field, column, control)?
                else {
                    return Ok(None);
                };
                selected
                    .snapshot_with_diskann_changes(&changes.without_unselected_documents(), control)
            })();
            return retained
                .map_err(|error| snapshot_error("complete physical vector selection", &error));
        }
        let Some(source_field) = self
            .layout
            .source_name(field)
            .or_else(|| column.is_none().then_some(field))
        else {
            return Ok(None);
        };
        let source_column = self
            .columns
            .iter()
            .find(|column| column.name == source_field);
        if source_column.map(|column| &column.ty) != column.map(|column| &column.ty) {
            return Ok(None);
        }
        let Some(base) = self
            .indexes
            .index(source_field)
            .filter(|index| index.dimensions() == dimensions)
        else {
            return Ok(None);
        };
        let Some(selected) = schema.vector_dimensions.index(field) else {
            return Ok(None);
        };
        let retained = (|| {
            let Some(base_source) = base.diskann_read_snapshot(control)? else {
                return Ok(None);
            };
            let Some(selected_source) = selected.diskann_read_snapshot(control)? else {
                return Ok(None);
            };
            if !base_source.same_lineage(&selected_source) {
                return Ok(None);
            }
            if !self.changes.has_changes() {
                return base.snapshot_with_control(control).map(Some);
            }
            let Some(changes) = self.changes.diskann_read_changes(field, column, control)? else {
                return Ok(None);
            };
            base.snapshot_with_diskann_changes(&changes, control)
        })();
        retained.map_err(|error| snapshot_error("physical vector selection", &error))
    }
}
