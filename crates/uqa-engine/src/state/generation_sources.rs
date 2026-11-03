//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction data supplies one source domain for matching table and catalog generations.

use super::{DurableCatalogSnapshot, SnapshotTableSources};
use crate::TableDataSnapshot;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::{
    security::roles::persistence::RoleCatalogSnapshot, sequence::snapshot::SequenceReadSnapshot,
};
use uqa_storage::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureRegistry},
    StorageBackendResult,
};

pub(crate) struct SnapshotSources {
    // Retire membership before the catalog sources it selects.
    tables: SnapshotTableSources,
    sequences: MemoryCaptureCell<SequenceReadSnapshot>,
}

impl SnapshotSources {
    pub(crate) fn new(
        registry: MemoryCaptureRegistry,
        tables: &BTreeMap<RelationIdentity, TableDataSnapshot>,
        durable: &DurableCatalogSnapshot,
    ) -> StorageBackendResult<Self> {
        let sequences = registry.cell(SequenceReadSnapshot {
            sequences: durable.sequences.clone(),
            object_ids: durable.sequence_object_ids.clone(),
            persistence: durable.sequence_persistence.clone(),
            security: durable.sequence_security.clone(),
            catalog_oids: durable.sequence_catalog_oids.clone(),
            roles: RoleCatalogSnapshot {
                roles: durable.roles.clone(),
                memberships: durable.role_memberships.clone(),
            },
        })?;
        let mut sources = Self {
            tables: SnapshotTableSources::new(registry, tables)?,
            sequences,
        };
        sources.tables.select(&sources.sequences.key()?)?;
        Ok(sources)
    }
}

#[cfg(test)]
type SequenceKey = uqa_storage::memory_capture::MemoryCaptureKey<
    uqa_storage::memory_capture::CapturedMemoryValue<
        Vec<uqa_execution::catalog::sequence::capture::CapturedSequence>,
    >,
>;

#[cfg(test)]
pub(crate) struct SnapshotCut {
    pub(crate) tables: super::SnapshotTableCut,
    pub(crate) sequences: uqa_core::memory::Budgeted<SequenceKey>,
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
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<SnapshotCut> {
        let key = self.sequences.key()?;
        let memory = control
            .memory()
            .reserve(size_of::<SequenceKey>() + key.retained_source_bytes())?;
        let tables = self.tables.capture(control)?;
        Ok(SnapshotCut {
            tables,
            sequences: uqa_core::memory::Budgeted::new(key, memory),
        })
    }
}
