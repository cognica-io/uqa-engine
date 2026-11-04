//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction data supplies one source domain for matching table and catalog generations.

use super::{DurableCatalogSnapshot, SnapshotTableSources};
use crate::{NontransactionalSequenceSource, TableDataSnapshot};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::{
    comments::snapshot::{CommentCaptureSource, CommentTableSource},
    foreign::capture::ForeignTableCaptureSource,
    security::roles::persistence::RoleCatalogSnapshot,
    sequence::{capture::SequenceCaptureSource, snapshot::SequenceReadSnapshot},
    table_capture::TableMetadataCaptureSource,
};
use uqa_storage::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureRegistry},
    StorageBackendResult,
};

mod catalogs;
mod resources;

pub(crate) struct SnapshotSources {
    // Retire membership before the catalog sources it selects.
    tables: SnapshotTableSources,
    sequences: NontransactionalSequenceSource,
    foreign: MemoryCaptureCell<ForeignTableCaptureSource>,
    metadata: MemoryCaptureCell<TableMetadataCaptureSource>,
    comments: MemoryCaptureCell<CommentCaptureSource>,
    catalogs: catalogs::CatalogSources,
    resources: resources::ResourceSources,
}

impl SnapshotSources {
    pub(crate) fn new(
        registry: MemoryCaptureRegistry,
        tables: &BTreeMap<RelationIdentity, TableDataSnapshot>,
        durable: &DurableCatalogSnapshot,
        temporary_namespace: Option<uqa_sql::catalog::temporary_namespace::TemporaryNamespaceOids>,
    ) -> StorageBackendResult<Self> {
        let sequences = std::sync::Arc::new(registry.cell(SequenceCaptureSource::new(
            SequenceReadSnapshot {
                catalog: durable.sequences.clone(),
                roles: RoleCatalogSnapshot {
                    roles: durable.roles.clone(),
                    memberships: durable.role_memberships.clone(),
                },
            },
        ))?);
        let catalogs = catalogs::CatalogSources::new(&registry, durable)?;
        let resources = resources::ResourceSources::new(&registry, durable)?;
        let foreign = registry.cell(ForeignTableCaptureSource {
            catalog: durable.foreign_catalog.clone(),
        })?;
        let metadata = registry.cell(TableMetadataCaptureSource::new(
            tables
                .iter()
                .map(|(name, table)| (name.clone(), table.persistence)),
            durable.catalog_indexes.clone(),
            durable.table_field_analyzers.clone(),
        ))?;
        let comments = registry.cell(
            CommentCaptureSource::new(
                &crate::Engine::catalog_definition_snapshot(durable),
                tables.iter().map(|(relation, table)| CommentTableSource {
                    relation,
                    object_id: table.object_id,
                    catalog_oids: table.catalog_oids,
                    persistence: table.persistence,
                    columns: &table.columns,
                    checks: &table.table_checks,
                    keys: &table.key_constraints,
                    foreign_keys: &table.foreign_keys,
                    hierarchy: &table.hierarchy,
                }),
                temporary_namespace,
            )
            .map_err(|error| {
                uqa_storage::StorageBackendError::backend("comment catalog capture", error)
            })?,
        )?;
        let mut sources = Self {
            tables: SnapshotTableSources::new(registry, tables)?,
            sequences,
            foreign,
            metadata,
            comments,
            catalogs,
            resources,
        };
        sources.tables.select(&sources.sequences.key()?)?;
        sources.tables.select(&sources.foreign.key()?)?;
        sources.tables.select(&sources.metadata.key()?)?;
        sources.tables.select(&sources.comments.key()?)?;
        sources.catalogs.select(&mut sources.tables)?;
        sources.resources.select(&mut sources.tables)?;
        Ok(sources)
    }

    pub(crate) fn sequence_history(&self) -> NontransactionalSequenceSource {
        self.sequences.clone()
    }
}

#[cfg(test)]
type SequenceKey = uqa_storage::memory_capture::MemoryCaptureKey<
    uqa_storage::memory_capture::CapturedMemoryValue<
        uqa_execution::catalog::sequence::capture::CapturedSequences,
    >,
>;

#[cfg(test)]
type ForeignKey = uqa_storage::memory_capture::MemoryCaptureKey<
    uqa_storage::memory_capture::CapturedMemoryValue<
        uqa_execution::catalog::foreign::registry::CapturedForeignCatalog,
    >,
>;

#[cfg(test)]
type MetadataKey = uqa_storage::memory_capture::MemoryCaptureKey<
    uqa_storage::memory_capture::CapturedMemoryValue<
        uqa_execution::catalog::table_capture::CapturedTableMetadata,
    >,
>;

#[cfg(test)]
type CommentKey = uqa_storage::memory_capture::MemoryCaptureKey<
    uqa_storage::memory_capture::CapturedMemoryValue<
        uqa_execution::catalog::comments::CapturedComments,
    >,
>;

#[cfg(test)]
pub(crate) struct SnapshotCut {
    pub(crate) tables: super::SnapshotTableCut,
    pub(crate) catalog_oids: uqa_execution::catalog::identity::CapturedCatalogOidPosition,
    pub(crate) sequences: uqa_core::memory::Budgeted<SequenceKey>,
    pub(crate) foreign: uqa_core::memory::Budgeted<ForeignKey>,
    pub(crate) metadata: uqa_core::memory::Budgeted<MetadataKey>,
    pub(crate) comments: uqa_core::memory::Budgeted<CommentKey>,
    pub(crate) catalogs: uqa_core::memory::Budgeted<catalogs::CatalogKeys>,
    pub(crate) resources: uqa_core::memory::Budgeted<resources::ResourceKeys>,
}

#[cfg(test)]
impl std::ops::Deref for SnapshotCut {
    type Target = super::SnapshotTableCut;

    fn deref(&self) -> &Self::Target {
        &self.tables
    }
}

#[cfg(test)]
impl SnapshotSources {
    pub(crate) fn capture(
        &self,
        catalog_oids: &uqa_execution::catalog::identity::CatalogOidCounter,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<SnapshotCut> {
        let key = self.sequences.key()?;
        let memory = control
            .memory()
            .reserve(size_of::<SequenceKey>() + key.retained_source_bytes())?;
        let catalogs = self.catalogs.keys(control)?;
        let resources = self.resources.keys(control)?;
        let foreign = self.foreign.key()?;
        let foreign_memory = control
            .memory()
            .reserve(size_of::<ForeignKey>() + foreign.retained_source_bytes())?;
        let metadata = self.metadata.key()?;
        let metadata_memory = control
            .memory()
            .reserve(size_of::<MetadataKey>() + metadata.retained_source_bytes())?;
        let comments = self.comments.key()?;
        let comment_memory = control
            .memory()
            .reserve(size_of::<CommentKey>() + comments.retained_source_bytes())?;
        let (tables, catalog_oids) =
            catalog_oids.capture_with(control, || self.tables.capture(control))?;
        Ok(SnapshotCut {
            tables,
            catalog_oids,
            sequences: uqa_core::memory::Budgeted::new(key, memory),
            foreign: uqa_core::memory::Budgeted::new(foreign, foreign_memory),
            metadata: uqa_core::memory::Budgeted::new(metadata, metadata_memory),
            comments: uqa_core::memory::Budgeted::new(comments, comment_memory),
            catalogs,
            resources,
        })
    }
}
