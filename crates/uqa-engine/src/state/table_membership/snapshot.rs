//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction values and live sources use distinct capture identities and domains.

use super::{Membership, TableSources};
use crate::TableDataSnapshot;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, RelationIdentity};
use uqa_storage::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureRegistry, MemoryCaptureSources},
    StorageBackendResult,
};

pub(crate) struct SnapshotTableSources {
    registry: MemoryCaptureRegistry,
    selection: MemoryCaptureSources,
    membership: MemoryCaptureCell<Membership>,
}

impl SnapshotTableSources {
    pub(crate) fn new(
        registry: MemoryCaptureRegistry,
        tables: &BTreeMap<RelationIdentity, TableDataSnapshot>,
    ) -> StorageBackendResult<Self> {
        let owner = MemoryBudget::new(usize::MAX);
        let mut sources = Self {
            selection: registry.sources(&owner),
            membership: MemoryCaptureCell::new(Membership::new()),
            registry,
        };
        sources.membership.register(&sources.registry)?;
        sources.selection = sources.selection.with_source(&sources.membership.key()?)?;
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let name = name.clone();
            let entry = TableSources::from_snapshot(table, &name)?;
            sources.selection = entry.select(&sources.selection)?;
            sources.selection = table
                .vector_indexes
                .read()
                .capture_sources(&sources.selection)?;
            let mut membership = sources.membership.write();
            *membership = membership.with_source(name, entry)?;
        }
        Ok(sources)
    }

    #[cfg(test)]
    pub(crate) fn capture(
        &self,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<SnapshotTableCut> {
        let cut = self.registry.capture_selected(&self.selection, control)?;
        let membership = self.membership.captured(&cut)?;
        Ok(SnapshotTableCut { cut, membership })
    }
}

#[cfg(test)]
pub(crate) struct SnapshotTableCut {
    pub(crate) cut: uqa_storage::memory_capture::MemoryCapture,
    membership: std::sync::Arc<uqa_core::memory::Budgeted<super::CapturedTableMembership>>,
}

#[cfg(test)]
impl SnapshotTableCut {
    pub(crate) fn assert_unread(&self) {
        for (_, source) in &self.membership.get().unwrap().0.sources {
            source.assert_unread(&self.cut);
        }
    }

    pub(crate) fn names(&self) -> StorageBackendResult<Vec<RelationIdentity>> {
        self.cut.check()?;
        Ok(self
            .membership
            .get()?
            .0
            .sources
            .iter()
            .map(|(name, _)| name.clone())
            .collect())
    }

    pub(crate) fn table(
        &self,
        name: &RelationIdentity,
    ) -> StorageBackendResult<super::sources::CapturedTable> {
        let (membership, identity) = self.membership.get()?;
        assert!(identity.matches(&self.cut));
        membership
            .sources
            .get(name)
            .ok_or_else(|| {
                uqa_storage::StorageBackendError::Other("table is absent from snapshot".into())
            })?
            .capture(&self.cut)
    }
}
