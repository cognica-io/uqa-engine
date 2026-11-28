//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table membership retains source identities, not mutable table owners.

use std::{
    ops::Deref,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::MappedRwLockReadGuard;
use uqa_core::{
    memory::{
        Budgeted, BudgetedSharedMap, BudgetedSharedMapSnapshot, MemoryBudget, MemoryError,
        OwnedMap, RetainedValue,
    },
    RelationIdentity,
};
use uqa_storage::{
    memory_capture::{
        CapturedMemoryValue, MemoryCaptureCell, MemoryCaptureIdentity, MemoryCaptureRegistry,
        MemoryCaptureSource, MemoryCaptureWriteGuard,
    },
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

use crate::TableState;

mod publication;
use publication::Publication;
mod sources;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(crate) use sources::CapturedTable;
use sources::TableSources;
mod input;
pub(crate) use input::TableRegistryInput;
mod snapshot;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(crate) use snapshot::SnapshotTableCut;
pub(crate) use snapshot::SnapshotTableSources;

#[cfg(test)]
mod tests;

pub(crate) type TableRegistryMap = OwnedMap<RelationIdentity, Arc<TableState>>;
type Tables = TableRegistryMap;

#[derive(Clone)]
struct Membership {
    sources: BudgetedSharedMapSnapshot<RelationIdentity, TableSources>,
    memory: MemoryBudget,
    payload_bytes: usize,
}

impl Membership {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            sources: BudgetedSharedMap::new(memory).snapshot(),
            memory: memory.clone(),
            payload_bytes: 0,
        }
    }

    fn with_table(
        &self,
        relation: &RelationIdentity,
        table: &TableState,
    ) -> StorageBackendResult<Self> {
        if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return self.without_table(relation);
        }
        let relation = relation.clone();
        let source = TableSources::new(table, &relation)?;
        self.with_source(relation, source)
    }

    fn with_source(
        &self,
        relation: RelationIdentity,
        source: TableSources,
    ) -> StorageBackendResult<Self> {
        let previous = self
            .sources
            .get(&relation)
            .map_or(0, TableSources::payload_bytes);
        let payload_bytes = self
            .payload_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(source.payload_bytes()))
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(Self {
            sources: self.sources.with_insert(relation, source, &self.memory)?,
            memory: self.memory.clone(),
            payload_bytes,
        })
    }

    fn without_table(&self, relation: &RelationIdentity) -> StorageBackendResult<Self> {
        let previous = self
            .sources
            .get(relation)
            .map_or(0, TableSources::payload_bytes);
        Ok(Self {
            sources: self.sources.with_remove(relation, &self.memory)?,
            memory: self.memory.clone(),
            payload_bytes: self.payload_bytes - previous,
        })
    }

    fn with_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Self> {
        let Some(previous) = self.sources.get(from) else {
            // Temporary entries have no durable source identities to rename.
            return Ok(Self {
                sources: self.sources.clone(),
                memory: self.memory.clone(),
                payload_bytes: self.payload_bytes,
            });
        };
        let to = to.clone();
        let source = previous.with_name(&to)?;
        let payload_bytes = self
            .payload_bytes
            .checked_sub(previous.payload_bytes())
            .and_then(|bytes| bytes.checked_add(source.payload_bytes()))
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(Self {
            sources: self.sources.with_remove(from, &self.memory)?.with_insert(
                to,
                source,
                &self.memory,
            )?,
            memory: self.memory.clone(),
            payload_bytes,
        })
    }

    fn snapshot(&self) -> MembershipSnapshot {
        MembershipSnapshot {
            sources: self.sources.clone(),
            payload_bytes: self.payload_bytes,
        }
    }

    fn restore(&mut self, snapshot: &MembershipSnapshot) {
        self.sources.clone_from(&snapshot.sources);
        self.payload_bytes = snapshot.payload_bytes;
    }
}

pub(crate) struct MembershipSnapshot {
    sources: BudgetedSharedMapSnapshot<RelationIdentity, TableSources>,
    payload_bytes: usize,
}

pub(crate) struct PersistentTableRegistrySnapshot<'a> {
    registry: &'a TableRegistry,
    live: RetainedValue<Tables>,
}

impl PersistentTableRegistrySnapshot<'_> {
    pub(crate) fn tables(&self) -> &Tables {
        &self.live
    }

    pub(crate) fn restore(&self) {
        let mut state = self.registry.cell.write();
        if !RetainedValue::ptr_eq(&state.live, &self.live) {
            state.live.clone_from(&self.live);
            self.registry.view_revision.fetch_add(1, Ordering::AcqRel);
        }
    }
}

struct MemoryTables {
    membership: Membership,
    publication: Publication,
}

struct State {
    live: RetainedValue<Tables>,
    memory: Option<MemoryTables>,
}

impl MemoryCaptureSource for State {
    type Frozen = CapturedTableMembership;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        Err(StorageBackendError::Other(
            "table membership capture requires its original cut".into(),
        ))
    }

    fn freeze_at(
        &self,
        control: &StorageReadControl,
        identity: &MemoryCaptureIdentity,
    ) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let memory = self.memory.as_ref().ok_or_else(|| {
            StorageBackendError::Other("persistent table membership has no memory capture".into())
        })?;
        memory.membership.freeze_at(control, identity)
    }
}

impl MemoryCaptureSource for Membership {
    type Frozen = CapturedTableMembership;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        Err(StorageBackendError::Other(
            "table membership capture requires its original cut".into(),
        ))
    }

    fn freeze_at(
        &self,
        control: &StorageReadControl,
        identity: &MemoryCaptureIdentity,
    ) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let snapshot = self.snapshot();
        let bytes = snapshot
            .sources
            .structural_bytes()?
            .checked_add(snapshot.payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        CapturedMemoryValue::new(Budgeted::new((snapshot, identity.clone()), memory), control)
    }
}

type CapturedTableMembership = CapturedMemoryValue<(MembershipSnapshot, MemoryCaptureIdentity)>;

pub(crate) struct TableCatalogRevision {
    registry: Arc<AtomicU64>,
    version: u64,
}

impl PartialEq for TableCatalogRevision {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && Arc::ptr_eq(&self.registry, &other.registry)
    }
}

impl Eq for TableCatalogRevision {}

pub(crate) struct TableRegistry {
    view_revision: Arc<AtomicU64>,
    cell: MemoryCaptureCell<State>,
    _image_memory: Option<uqa_core::memory::MemoryReservation>,
}

impl TableRegistry {
    pub(crate) fn memory(registry: &MemoryCaptureRegistry) -> StorageBackendResult<Self> {
        Self::from_native_tables(
            std::iter::empty(),
            Some(registry),
            &StorageReadControl::with_limit(usize::MAX),
        )
    }

    /// Construct complete native membership through the same consuming input used by incremental table restoration. Ordinary empty memory state uses this boundary with its own unconstrained allocation domain.
    pub(crate) fn from_native_tables(
        tables: impl IntoIterator<Item = (Budgeted<RelationIdentity>, Arc<TableState>)>,
        registry: Option<&MemoryCaptureRegistry>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut input = TableRegistryInput::new(registry, control)?;
        for (name, table) in tables {
            input = input.add_table(name, table)?;
        }
        input.finish()
    }

    pub(crate) fn revision(&self) -> TableCatalogRevision {
        TableCatalogRevision {
            registry: Arc::clone(&self.view_revision),
            version: self.view_revision.load(Ordering::Acquire),
        }
    }

    pub(crate) fn persistent() -> Self {
        Self {
            view_revision: Arc::default(),
            cell: MemoryCaptureCell::new(State {
                live: RetainedValue::new(Tables::new()),
                memory: None,
            }),
            _image_memory: None,
        }
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, Tables> {
        parking_lot::RwLockReadGuard::map(self.cell.read(), |state| &*state.live)
    }

    pub(crate) fn write(&self) -> TableRegistryWrite<'_> {
        TableRegistryWrite {
            state: self.cell.write(),
            view_revision: &self.view_revision,
        }
    }

    #[cfg(test)]
    pub(crate) fn try_write(&self) -> Option<TableRegistryWrite<'_>> {
        self.cell.try_write().map(|state| TableRegistryWrite {
            state,
            view_revision: &self.view_revision,
        })
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.cell.is_locked()
    }

    pub(crate) fn memory_snapshot(&self) -> Option<MembershipSnapshot> {
        let state = self.cell.read();
        state
            .memory
            .as_ref()
            .map(|memory| memory.membership.snapshot())
    }

    pub(crate) fn restore_memory(
        &self,
        snapshot: &MembershipSnapshot,
        tables: impl Iterator<Item = (RelationIdentity, Arc<TableState>)>,
    ) -> StorageBackendResult<()> {
        // The transaction owner restores physical sources before publishing the matching live lookup and exact source groups. Undo retains its original immutable name/metadata root.
        let live = RetainedValue::new(tables.collect());
        let mut state = self.cell.write();
        let memory = state.memory.as_ref().expect("memory registry undo owner");
        let mut membership = memory.membership.clone();
        membership.restore(snapshot);
        let publication = memory.publication.replacement(&membership, &live)?;
        state.memory = Some(MemoryTables {
            membership,
            publication,
        });
        state.live = live;
        self.view_revision.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Persistent catalog reloads restore retained logical handles independently of memory's physical undo and source publication.
    pub(crate) fn persistent_snapshot(
        &self,
    ) -> StorageBackendResult<PersistentTableRegistrySnapshot<'_>> {
        let state = self.cell.read();
        if state.memory.is_some() {
            return Err(StorageBackendError::Other(
                "memory table restoration requires physical transaction undo".into(),
            ));
        }
        Ok(PersistentTableRegistrySnapshot {
            registry: self,
            live: state.live.clone(),
        })
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(crate) fn snapshot_sources(
        &self,
    ) -> StorageBackendResult<(MemoryCaptureRegistry, SnapshotTableSources)> {
        let state = self.cell.read();
        let memory = state.memory.as_ref().ok_or_else(|| {
            StorageBackendError::Other("persistent table membership has no memory capture".into())
        })?;
        let registry = memory.publication.registry().clone();
        let sources = SnapshotTableSources::live(
            registry.clone(),
            memory.publication.selected(),
            self.cell.key()?,
        )?;
        Ok((registry, sources))
    }

    pub(crate) fn live_snapshot(&self) -> RetainedValue<Tables> {
        self.cell.read().live.clone()
    }

    #[cfg(all(test, any(windows, all(unix, not(target_os = "emscripten")))))]
    pub(crate) fn key(
        &self,
    ) -> StorageBackendResult<uqa_storage::memory_capture::MemoryCaptureKey<CapturedTableMembership>>
    {
        self.cell.key()
    }

    #[cfg(test)]
    pub(crate) fn captured(
        &self,
        cut: &uqa_storage::memory_capture::MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<CapturedTableMembership>>> {
        self.cell.captured(cut)
    }
}

pub(crate) struct TableRegistryWrite<'a> {
    state: MemoryCaptureWriteGuard<'a, State>,
    view_revision: &'a Arc<AtomicU64>,
}

impl Deref for TableRegistryWrite<'_> {
    type Target = Tables;
    fn deref(&self) -> &Tables {
        &self.state.live
    }
}

impl TableRegistryWrite<'_> {
    pub(crate) fn insert(
        &mut self,
        relation: RelationIdentity,
        table: Arc<TableState>,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if let Some(memory) = &mut self.state.memory {
            let membership = memory.membership.with_table(&relation, &table)?;
            memory
                .publication
                .insert(&relation, &table, membership.sources.get(&relation))?;
            memory.membership = membership;
        }
        table.track_catalog_view_revision(self.view_revision);
        let previous = RetainedValue::make_mut(&mut self.state.live).insert(relation, table);
        self.view_revision.fetch_add(1, Ordering::AcqRel);
        Ok(previous)
    }

    pub(crate) fn remove(
        &mut self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if let Some(memory) = &mut self.state.memory {
            let membership = memory.membership.without_table(relation)?;
            memory.publication.remove(relation)?;
            memory.membership = membership;
        }
        let removed = RetainedValue::make_mut(&mut self.state.live).remove(relation);
        if removed.is_some() {
            self.view_revision.fetch_add(1, Ordering::AcqRel);
        }
        Ok(removed)
    }

    pub(crate) fn rename(
        &mut self,
        from: &RelationIdentity,
        to: RelationIdentity,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if self.state.live.contains_key(&to) {
            return Ok(None);
        }
        let Some(table) = self.state.live.get(from).cloned() else {
            return Ok(None);
        };
        if let Some(memory) = &mut self.state.memory {
            let membership = memory.membership.with_rename(from, &to)?;
            memory.publication.rename(from, &to);
            memory.membership = membership;
        }
        let live = RetainedValue::make_mut(&mut self.state.live);
        live.remove(from);
        live.insert(to, table.clone());
        self.view_revision.fetch_add(1, Ordering::AcqRel);
        Ok(Some(table))
    }

    pub(crate) fn replace(&mut self, live: Tables) -> StorageBackendResult<()> {
        if let Some(memory) = &mut self.state.memory {
            let mut membership = Membership {
                sources: BudgetedSharedMap::new(&memory.membership.memory).snapshot(),
                memory: memory.membership.memory.clone(),
                payload_bytes: 0,
            };
            for (relation, table) in &live {
                membership = membership.with_table(relation, table)?;
            }
            let publication = memory.publication.replacement(&membership, &live)?;
            *memory = MemoryTables {
                membership,
                publication,
            };
        }
        for table in live.values() {
            table.track_catalog_view_revision(self.view_revision);
        }
        self.state.live = RetainedValue::new(live);
        self.view_revision.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn retain(
        &mut self,
        mut keep: impl FnMut(&RelationIdentity, &Arc<TableState>) -> bool,
    ) -> StorageBackendResult<()> {
        if self.state.memory.is_none() {
            let live = RetainedValue::make_mut(&mut self.state.live);
            let before = live.len();
            live.retain(|relation, table| keep(relation, table));
            if before != live.len() {
                self.view_revision.fetch_add(1, Ordering::AcqRel);
            }
            return Ok(());
        }
        let retained = self
            .state
            .live
            .iter()
            .filter(|(name, table)| keep(name, table))
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect();
        self.replace(retained)
    }
}
