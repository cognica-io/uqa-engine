//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table state supplies borrowed native properties and the SQL/Planner owners' definitions.

use super::readers::{selected, sql_error, ReadFn};
use crate::state::CapturedTable;
use std::sync::Arc;
use uqa_core::memory::Budgeted;
use uqa_sql::ast::{OnCommitAction, RelationPersistence};
use uqa_storage::{
    backup::{
        memory_table_metadata_reader, MemoryRelationIdentities, MemoryTableMetadata,
        MemoryTableProperties, MemoryTableStatistics,
    },
    image_io::{ImageOwner, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(super) const TABLE_METADATA_COUNT: u8 = 10;
pub(super) type TableOwner = Arc<Budgeted<CapturedTable>>;

pub(super) fn retain(
    table: CapturedTable,
    control: &StorageReadControl,
) -> StorageBackendResult<TableOwner> {
    control.check()?;
    let memory = control
        .memory()
        .reserve(size_of::<Budgeted<CapturedTable>>())?;
    Ok(Arc::new(Budgeted::new(table, memory)))
}

struct Properties(TableOwner);
impl ImageOwner for Properties {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        let table = &self.0;
        let properties = MemoryTableProperties {
            object_id: table.object_id,
            storage_generation: *table.storage_generation.get()?,
            catalog_oids: table.catalog_oids.map(|oids| MemoryRelationIdentities {
                relation: oids.relation,
                row_type: oids.row_type,
                array_type: oids.array_type,
                rule: oids.rule,
            }),
            persistence: match table.persistence {
                RelationPersistence::Permanent => 0,
                RelationPersistence::Unlogged => 1,
                RelationPersistence::Temporary => {
                    return Err(StorageBackendError::Other(
                        "temporary table is outside the captured image".into(),
                    ))
                }
            },
            on_commit: match table.on_commit {
                OnCommitAction::PreserveRows => 0,
                OnCommitAction::DeleteRows => 1,
                OnCommitAction::Drop => 2,
            },
            columns_declared: *table.columns_declared.get()?,
            maps_integer_keys: *table.maps_integer_keys.get()?,
            statistics: MemoryTableStatistics {
                loaded: *table.column_stats_loaded.get()?,
                dirty: *table.column_stats_dirty.get()?,
            },
            next_id: *table.next_id.get()?,
        };
        memory_table_metadata_reader(
            MemoryTableMetadata {
                properties,
                fts_fields: table.fts_fields.get()?,
                analyzer: table.analyzer.get()?,
                row_type_array_name: table.row_type_array_name.get()?.as_deref(),
            },
            control,
        )?
        .with_source_control(Some(&table.control), control)
    }
}

pub(super) fn image(
    table: &TableOwner,
    slot: u8,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<ImageReader<'static>>> {
    macro_rules! definition {
        ($field:ident, $reader:ident) => {
            selected(table.$field.clone(), control, |value, control| {
                let mut reader = uqa_sql::catalog::image::$reader(
                    value,
                    control.memory(),
                    control.cancellation(),
                )
                .map_err(sql_error)?;
                ImageReader::new(
                    ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                    control,
                )
            })?
        };
    }
    let reader = match slot {
        0 => ImageReader::from_owner(Properties(table.clone()), control)?,
        1 => selected(table.security.clone(), control, |value, control| {
            let mut reader = value
                .image_reader(control.cancellation())
                .map_err(sql_error)?;
            ImageReader::new(
                ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                control,
            )
        })?,
        2 => definition!(columns, columns_image_reader),
        3 => definition!(table_checks, table_checks_image_reader),
        4 => definition!(foreign_keys, foreign_keys_image_reader),
        5 => definition!(key_constraints, table_keys_image_reader),
        6 => definition!(hierarchy, table_hierarchy_image_reader),
        7 => definition!(dropped_attributes, dropped_attributes_image_reader),
        8 => selected(table.column_stats.clone(), control, |value, control| {
            let mut reader = value
                .image_reader(control.memory(), control.cancellation())
                .map_err(sql_error)?;
            ImageReader::new(
                ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                control,
            )
        })?,
        9 => selected(
            table.statistics_maintenance.clone(),
            control,
            |value, control| {
                let mut reader = value
                    .image_reader(control.cancellation())
                    .map_err(io_error)?;
                ImageReader::new(
                    ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(io_error)),
                    control,
                )
            },
        )?,
        _ => return Ok(None),
    };
    Ok(Some(reader))
}

fn io_error(error: uqa_core::image_io::ImageIoError) -> StorageBackendError {
    match error {
        uqa_core::image_io::ImageIoError::Memory(error) => error.into(),
        uqa_core::image_io::ImageIoError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("memory table image", error),
    }
}
