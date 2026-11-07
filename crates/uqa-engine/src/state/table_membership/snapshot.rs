//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original transaction sources and live publication share the same immutable cut interface.

use super::{CapturedTableMembership, Membership, TableSources};
use crate::TableDataSnapshot;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, RelationIdentity};
use uqa_storage::{
    memory_capture::{
        MemoryCaptureCell, MemoryCaptureKey, MemoryCaptureRegistry, MemoryCaptureSources,
    },
    StorageBackendResult,
};

pub(crate) struct SnapshotTableSources {
    registry: MemoryCaptureRegistry,
    selection: [MemoryCaptureSources; 2],
    membership: MembershipSource,
}

enum MembershipSource {
    Original(MemoryCaptureCell<Membership>),
    #[cfg(test)]
    Live(MemoryCaptureKey<CapturedTableMembership>),
}

impl MembershipSource {
    fn key(&self) -> StorageBackendResult<MemoryCaptureKey<CapturedTableMembership>> {
        match self {
            Self::Original(source) => source.key(),
            #[cfg(test)]
            Self::Live(key) => Ok(key.clone()),
        }
    }
}

impl SnapshotTableSources {
    pub(crate) fn registry(&self) -> &MemoryCaptureRegistry {
        &self.registry
    }

    pub(crate) fn select<F: Send + Sync + 'static>(
        &mut self,
        source: &MemoryCaptureKey<F>,
    ) -> StorageBackendResult<()> {
        self.selection[1] = self.selection[1].with_source(source)?;
        Ok(())
    }

    pub(crate) fn new(
        registry: MemoryCaptureRegistry,
        tables: &BTreeMap<RelationIdentity, TableDataSnapshot>,
    ) -> StorageBackendResult<Self> {
        let owner = MemoryBudget::new(usize::MAX);
        let membership = registry.cell(Membership::new())?;
        let mut selection = registry.sources(&owner);
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let name = name.clone();
            let entry = TableSources::from_snapshot(table, &name)?;
            selection = entry.select(&selection)?;
            selection = table.vector_indexes.read().capture_sources(&selection)?;
            let mut root = membership.write();
            *root = root.with_source(name, entry)?;
        }
        let mut sources = Self {
            selection: [selection, registry.sources(&owner)],
            registry,
            membership: MembershipSource::Original(membership),
        };
        sources.select(&sources.membership.key()?)?;
        Ok(sources)
    }

    #[cfg(test)]
    pub(super) fn live(
        registry: MemoryCaptureRegistry,
        selection: MemoryCaptureSources,
        membership: MemoryCaptureKey<CapturedTableMembership>,
    ) -> StorageBackendResult<Self> {
        let additional = registry
            .sources(&MemoryBudget::new(usize::MAX))
            .with_source(&membership)?;
        Ok(Self {
            registry,
            selection: [selection, additional],
            membership: MembershipSource::Live(membership),
        })
    }

    #[cfg(test)]
    pub(crate) fn capture(
        &self,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<SnapshotTableCut> {
        let cut = self.registry.capture_selected_union(
            &self.selection[0],
            &self.selection[1],
            control,
        )?;
        let membership = self.membership.key()?.captured(&cut)?;
        Ok(SnapshotTableCut { cut, membership })
    }
}

#[cfg(test)]
pub(crate) struct SnapshotTableCut {
    pub(crate) cut: uqa_storage::memory_capture::MemoryCapture,
    membership: std::sync::Arc<uqa_core::memory::Budgeted<CapturedTableMembership>>,
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
