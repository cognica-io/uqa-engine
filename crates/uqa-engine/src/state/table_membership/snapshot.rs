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
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    Live(MemoryCaptureKey<CapturedTableMembership>),
}

impl MembershipSource {
    fn key(&self) -> StorageBackendResult<MemoryCaptureKey<CapturedTableMembership>> {
        match self {
            Self::Original(source) => source.key(),
            #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
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
        let membership = registry.cell(Membership::new(&owner))?;
        let mut selection = registry.sources(&owner);
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let name = name.clone();
            let entry = TableSources::from_snapshot(table, &name)?;
            selection = entry.select(&selection)?;
            selection = table.vector_indexes.read().capture_sources(&selection)?;
            selection = table.value_indexes.read().capture_sources(&selection)?;
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

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
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

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(crate) fn capture(
        &self,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<SnapshotTableCut> {
        let cut = self.registry.capture_selected_union(
            &self.selection[0],
            &self.selection[1],
            control,
        )?;
        let key = self.membership.key()?;
        let image_generation = key.image_generation(&cut)?;
        let membership = key.captured(&cut)?;
        Ok(SnapshotTableCut {
            cut,
            image_generation,
            membership,
        })
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[derive(Clone)]
pub(crate) struct SnapshotTableCut {
    pub(crate) cut: uqa_storage::memory_capture::MemoryCapture,
    pub(crate) image_generation: Option<uqa_storage::memory_capture::MemoryImageGeneration>,
    membership: std::sync::Arc<uqa_core::memory::Budgeted<CapturedTableMembership>>,
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
impl SnapshotTableCut {
    pub(crate) fn stored_expressions(
        &self,
    ) -> StorageBackendResult<impl Iterator<Item = StorageBackendResult<StoredTableExpressions>> + '_>
    {
        self.cut.check()?;
        let (membership, identity) = self.membership.get()?;
        assert!(identity.matches(&self.cut));
        Ok(membership.sources.iter().map(|(_, table)| {
            let columns = table.columns.captured(&self.cut)?;
            let checks = table.table_checks.captured(&self.cut)?;
            Ok((columns.get()?.clone(), checks.get()?.clone()))
        }))
    }

    #[cfg(test)]
    pub(crate) fn assert_unread(&self) {
        for (_, source) in &self.membership.get().unwrap().0.sources {
            source.assert_unread(&self.cut);
        }
    }

    #[cfg(test)]
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

    pub(crate) fn next_table(
        &self,
        after: Option<&RelationIdentity>,
    ) -> StorageBackendResult<Option<(&RelationIdentity, super::sources::CapturedTable)>> {
        self.cut.check()?;
        let (membership, identity) = self.membership.get()?;
        assert!(identity.matches(&self.cut));
        membership
            .sources
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(name, source)| Ok((name, source.capture(&self.cut)?)))
            .transpose()
    }

    #[cfg(test)]
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

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
type StoredTableExpressions = (
    uqa_sql::catalog::table_definition::TableDefinition<Vec<uqa_sql::ast::ColumnDef>>,
    uqa_sql::catalog::table_definition::TableDefinition<Vec<uqa_sql::ast::TableCheck>>,
);
