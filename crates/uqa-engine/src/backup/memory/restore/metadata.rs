//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table input transfers each decoded definition into its native immutable owner.

use crate::state::{CatalogGeneration, TableDefinitionCell, TableMetadataCell};
use std::io::Read;
use uqa_core::memory::Budgeted;
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::{
        image,
        relation_attributes::DroppedAttribute,
        security::BoundTableSecurity,
        table_definition::TableDefinition,
        table_metadata::{TableMetadata, TableMetadataValue},
    },
};
use uqa_storage::{
    backup::DecodedMemoryTableMetadata, read_control::StorageReadControl,
    statistics_maintenance::StatisticsMaintenance, StorageBackendError, StorageBackendResult,
};

/// An incomplete candidate is never exposed as a live table. Each transition consumes its previous owner, so failed decoding retires every earlier component.
pub(super) struct MetadataInput {
    properties: Budgeted<DecodedMemoryTableMetadata>,
    next: u8,
    security: Option<
        TableMetadataCell<
            BoundTableSecurity,
            uqa_sql::catalog::security::table_binding::CapturedTableSecurity,
        >,
    >,
    columns: Option<TableDefinitionCell<Vec<ColumnDef>>>,
    checks: Option<TableDefinitionCell<Vec<TableCheck>>>,
    foreign_keys: Option<TableMetadataCell<Vec<ForeignKey>>>,
    keys: Option<TableMetadataCell<Vec<TableKeyConstraint>>>,
    hierarchy: Option<TableDefinitionCell<TableHierarchy>>,
    dropped: Option<TableMetadataCell<Vec<DroppedAttribute>>>,
    statistics: Option<uqa_planner::ColumnStatistics>,
    maintenance: Option<StatisticsMaintenance>,
}

pub(super) struct Metadata {
    pub properties: uqa_storage::backup::MemoryTableProperties,
    pub fts_fields: TableMetadataCell<Vec<String>>,
    pub analyzer: uqa_analysis::AnalyzerDefinition,
    pub array_name: TableMetadataCell<Option<String>>,
    pub security: TableMetadataCell<
        BoundTableSecurity,
        uqa_sql::catalog::security::table_binding::CapturedTableSecurity,
    >,
    pub columns: TableDefinitionCell<Vec<ColumnDef>>,
    pub checks: TableDefinitionCell<Vec<TableCheck>>,
    pub foreign_keys: TableMetadataCell<Vec<ForeignKey>>,
    pub keys: TableMetadataCell<Vec<TableKeyConstraint>>,
    pub hierarchy: TableDefinitionCell<TableHierarchy>,
    pub dropped: TableMetadataCell<Vec<DroppedAttribute>>,
    pub statistics: uqa_planner::ColumnStatistics,
    pub maintenance: StatisticsMaintenance,
}

impl MetadataInput {
    pub(super) fn new(properties: Budgeted<DecodedMemoryTableMetadata>) -> Self {
        Self {
            properties,
            next: 1,
            security: None,
            columns: None,
            checks: None,
            foreign_keys: None,
            keys: None,
            hierarchy: None,
            dropped: None,
            statistics: None,
            maintenance: None,
        }
    }

    pub(super) fn read(
        mut self,
        slot: u8,
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if slot != self.next {
            return Err(invalid());
        }
        let memory = control.memory();
        let cancellation = control.cancellation();
        match slot {
            1 => {
                let value = BoundTableSecurity::read_image(reader, memory, cancellation)
                    .map_err(sql_error)?;
                self.security = Some(metadata(value, control)?);
            }
            2 => {
                let value =
                    TableDefinition::<Vec<ColumnDef>>::read_image(reader, memory, cancellation)
                        .map_err(sql_error)?;
                self.columns = Some(TableDefinitionCell::from_generation(
                    CatalogGeneration::with_budget(value, memory)?,
                ));
            }
            3 => {
                let value =
                    TableDefinition::<Vec<TableCheck>>::read_image(reader, memory, cancellation)
                        .map_err(sql_error)?;
                self.checks = Some(TableDefinitionCell::from_generation(
                    CatalogGeneration::with_budget(value, memory)?,
                ));
            }
            4 => {
                self.foreign_keys = Some(metadata(
                    image::read_foreign_keys_image(reader, memory, cancellation)
                        .map_err(sql_error)?,
                    control,
                )?);
            }
            5 => {
                self.keys = Some(metadata(
                    image::read_table_keys_image(reader, memory, cancellation)
                        .map_err(sql_error)?,
                    control,
                )?);
            }
            6 => {
                let value =
                    TableDefinition::<TableHierarchy>::read_image(reader, memory, cancellation)
                        .map_err(sql_error)?;
                self.hierarchy = Some(TableDefinitionCell::from_generation(
                    CatalogGeneration::with_budget(value, memory)?,
                ));
            }
            7 => {
                self.dropped = Some(metadata(
                    image::read_dropped_attributes_image(reader, memory, cancellation)
                        .map_err(sql_error)?,
                    control,
                )?);
            }
            8 => {
                self.statistics = Some(
                    uqa_planner::ColumnStatistics::read_image(reader, memory, cancellation)
                        .map_err(sql_error)?,
                );
            }
            9 => {
                self.maintenance = Some(
                    StatisticsMaintenance::read_image(reader, memory, cancellation).map_err(
                        |error| StorageBackendError::backend("memory table maintenance", error),
                    )?,
                );
            }
            _ => return Err(invalid()),
        }
        control.check()?;
        self.next += 1;
        Ok(self)
    }

    pub(super) fn finish(self, control: &StorageReadControl) -> StorageBackendResult<Metadata> {
        control.check()?;
        if self.next != uqa_storage::backup::MEMORY_IMAGE_TABLE_METADATA_COUNT {
            return Err(invalid());
        }
        let (properties, _header) = self.properties.into_parts();
        let analyzer = uqa_analysis::AnalyzerDefinition::from_image(
            properties.analyzer,
            control.cancellation(),
        )
        .map_err(|error| StorageBackendError::backend("memory table analyzer", error))?;
        let value = Metadata {
            properties: properties.properties,
            fts_fields: metadata(properties.fts_fields, control)?,
            array_name: metadata(properties.row_type_array_name, control)?,
            analyzer,
            security: self.security.expect("complete metadata sequence"),
            columns: self.columns.expect("complete metadata sequence"),
            checks: self.checks.expect("complete metadata sequence"),
            foreign_keys: self.foreign_keys.expect("complete metadata sequence"),
            keys: self.keys.expect("complete metadata sequence"),
            hierarchy: self.hierarchy.expect("complete metadata sequence"),
            dropped: self.dropped.expect("complete metadata sequence"),
            statistics: self.statistics.expect("complete metadata sequence"),
            maintenance: self.maintenance.expect("complete metadata sequence"),
        };
        control.check()?;
        Ok(value)
    }
}

fn metadata<T: TableMetadataValue, F: Send + Sync + 'static>(
    value: Budgeted<T>,
    control: &StorageReadControl,
) -> StorageBackendResult<TableMetadataCell<T, F>> {
    let value = TableMetadata::from_admitted(value, control.cancellation()).map_err(sql_error)?;
    Ok(TableMetadataCell::from_generation(
        CatalogGeneration::with_budget(value, control.memory())?,
    ))
}

pub(super) fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("memory table image", error)
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("incomplete or unordered memory table metadata".into())
}
