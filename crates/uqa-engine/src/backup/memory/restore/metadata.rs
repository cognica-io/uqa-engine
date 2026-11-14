//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table input transfers each decoded definition into its native immutable owner.

use std::io::Read;
use uqa_core::{image_io::ImageDecodeWindow, memory::Budgeted};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::{
        relation_attributes::DroppedAttribute, security::BoundTableSecurity,
        table_definition::TableDefinition, table_metadata::TableMetadata,
    },
};
use uqa_storage::{
    read_control::StorageReadControl, statistics_maintenance::StatisticsMaintenance,
    StorageBackendError, StorageBackendResult,
};

mod definitions;
use definitions::DefinitionInput;
pub(super) mod header;
use header::Header;

/// An incomplete candidate is never exposed as a live table. Each transition consumes its previous owner, so failed decoding retires every earlier component.
pub(super) struct MetadataInput {
    properties: Budgeted<Header>,
    next: u8,
    input: Option<DefinitionInput>,
    security: Option<TableMetadata<BoundTableSecurity>>,
    columns: Option<TableDefinition<Vec<ColumnDef>>>,
    checks: Option<TableDefinition<Vec<TableCheck>>>,
    foreign_keys: Option<TableMetadata<Vec<ForeignKey>>>,
    keys: Option<TableMetadata<Vec<TableKeyConstraint>>>,
    hierarchy: Option<TableDefinition<TableHierarchy>>,
    dropped: Option<TableMetadata<Vec<DroppedAttribute>>>,
    statistics: Option<uqa_planner::ColumnStatistics>,
    maintenance: Option<StatisticsMaintenance>,
}

pub(super) struct Metadata {
    pub properties: uqa_storage::backup::MemoryTableProperties,
    pub fts_fields: TableMetadata<Vec<String>>,
    pub analyzer: uqa_analysis::AnalyzerDefinition,
    pub array_name: TableMetadata<Option<String>>,
    pub security: TableMetadata<BoundTableSecurity>,
    pub columns: TableDefinition<Vec<ColumnDef>>,
    pub checks: TableDefinition<Vec<TableCheck>>,
    pub foreign_keys: TableMetadata<Vec<ForeignKey>>,
    pub keys: TableMetadata<Vec<TableKeyConstraint>>,
    pub hierarchy: TableDefinition<TableHierarchy>,
    pub dropped: TableMetadata<Vec<DroppedAttribute>>,
    pub statistics: uqa_planner::ColumnStatistics,
    pub maintenance: StatisticsMaintenance,
}

impl MetadataInput {
    pub(super) fn new(properties: Budgeted<Header>) -> Self {
        Self {
            properties,
            next: 1,
            input: None,
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
        window: ImageDecodeWindow,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Self, bool)> {
        control.check()?;
        if slot != self.next {
            return Err(invalid());
        }
        let mut input = match self.input.take() {
            Some(input) => input,
            None => DefinitionInput::new(slot, control)?,
        };
        let complete = input.advance(&mut self, reader, window)?;
        control.check()?;
        if complete {
            self.next += 1;
        } else {
            self.input = Some(input);
        }
        Ok((self, complete))
    }

    pub(super) fn finish(self, control: &StorageReadControl) -> StorageBackendResult<Metadata> {
        control.check()?;
        if self.next != uqa_storage::backup::MEMORY_IMAGE_TABLE_METADATA_COUNT {
            return Err(invalid());
        }
        let value = self.properties.try_map(|properties| {
            let value = Metadata {
                properties: properties.properties,
                fts_fields: properties.fts_fields,
                array_name: properties.array_name,
                analyzer: properties.analyzer,
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
            Ok::<_, StorageBackendError>(value)
        })?;
        let (value, _header) = value.into_parts();
        Ok(value)
    }
}

pub(super) fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("memory table image", error)
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("incomplete or unordered memory table metadata".into())
}
