//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table membership retains source identities, not mutable table owners.

use std::{collections::BTreeMap, ops::Deref, sync::Arc};

use parking_lot::MappedRwLockReadGuard;
use uqa_core::{
    memory::{Budgeted, BudgetedSharedMap, BudgetedSharedMapSnapshot, MemoryBudget, MemoryError},
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
use sources::TableSources;
mod snapshot;
#[cfg(test)]
pub(crate) use snapshot::SnapshotTableCut;
pub(crate) use snapshot::SnapshotTableSources;

#[cfg(test)]
mod tests;

type Tables = BTreeMap<RelationIdentity, Arc<TableState>>;

#[derive(Clone)]
struct Membership {
    sources: BudgetedSharedMap<RelationIdentity, TableSources>,
    payload_bytes: usize,
}

impl Membership {
    fn new() -> Self {
        Self {
            sources: BudgetedSharedMap::new(&MemoryBudget::new(usize::MAX)),
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
            sources: self.sources.with_insert(relation, source)?,
            payload_bytes,
        })
    }

    fn without_table(&self, relation: &RelationIdentity) -> StorageBackendResult<Self> {
        let previous = self
            .sources
            .get(relation)
            .map_or(0, TableSources::payload_bytes);
        Ok(Self {
            sources: self.sources.with_remove(relation)?,
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
            sources: self.sources.with_remove(from)?.with_insert(to, source)?,
            payload_bytes,
        })
    }

    fn snapshot(&self) -> MembershipSnapshot {
        MembershipSnapshot {
            sources: self.sources.snapshot(),
            payload_bytes: self.payload_bytes,
        }
    }

    fn restore(&mut self, snapshot: &MembershipSnapshot) {
        self.sources.restore(&snapshot.sources);
        self.payload_bytes = snapshot.payload_bytes;
    }
}

pub(crate) struct MembershipSnapshot {
    sources: BudgetedSharedMapSnapshot<RelationIdentity, TableSources>,
    payload_bytes: usize,
}

pub(crate) struct PersistentTableRegistrySnapshot<'a> {
    registry: &'a TableRegistry,
    live: Arc<Tables>,
}

impl PersistentTableRegistrySnapshot<'_> {
    pub(crate) fn tables(&self) -> &Tables {
        &self.live
    }

    pub(crate) fn restore(&self) {
        self.registry.0.write().live.clone_from(&self.live);
    }
}

struct MemoryTables {
    membership: Membership,
    publication: Publication,
}

struct State {
    live: Arc<Tables>,
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

pub(crate) struct TableRegistry(MemoryCaptureCell<State>);

impl TableRegistry {
    pub(crate) fn memory(registry: &MemoryCaptureRegistry) -> StorageBackendResult<Self> {
        Ok(Self(registry.cell(State {
            live: Arc::new(Tables::new()),
            memory: Some(MemoryTables {
                membership: Membership::new(),
                publication: Publication::new(registry, &MemoryBudget::new(usize::MAX))?,
            }),
        })?))
    }

    pub(crate) fn persistent() -> Self {
        Self(MemoryCaptureCell::new(State {
            live: Arc::new(Tables::new()),
            memory: None,
        }))
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, Tables> {
        parking_lot::RwLockReadGuard::map(self.0.read(), |state| &*state.live)
    }

    pub(crate) fn write(&self) -> TableRegistryWrite<'_> {
        TableRegistryWrite(self.0.write())
    }

    #[cfg(test)]
    pub(crate) fn try_write(&self) -> Option<TableRegistryWrite<'_>> {
        self.0.try_write().map(TableRegistryWrite)
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    pub(crate) fn memory_snapshot(&self) -> Option<MembershipSnapshot> {
        let state = self.0.read();
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
        let live = Arc::new(tables.collect());
        let mut state = self.0.write();
        let memory = state.memory.as_ref().expect("memory registry undo owner");
        let mut membership = memory.membership.clone();
        membership.restore(snapshot);
        let publication = memory.publication.replacement(&membership, &live)?;
        state.memory = Some(MemoryTables {
            membership,
            publication,
        });
        state.live = live;
        Ok(())
    }

    /// Persistent catalog reloads restore retained logical handles independently of memory's physical undo and source publication.
    pub(crate) fn persistent_snapshot(
        &self,
    ) -> StorageBackendResult<PersistentTableRegistrySnapshot<'_>> {
        let state = self.0.read();
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

    #[cfg(test)]
    pub(crate) fn snapshot_sources(
        &self,
    ) -> StorageBackendResult<(MemoryCaptureRegistry, SnapshotTableSources)> {
        let state = self.0.read();
        let memory = state.memory.as_ref().ok_or_else(|| {
            StorageBackendError::Other("persistent table membership has no memory capture".into())
        })?;
        let registry = memory.publication.registry().clone();
        let sources = SnapshotTableSources::live(
            registry.clone(),
            memory.publication.selected(),
            self.0.key()?,
        )?;
        Ok((registry, sources))
    }

    #[cfg(test)]
    pub(crate) fn live_snapshot(&self) -> Arc<Tables> {
        self.0.read().live.clone()
    }

    #[cfg(test)]
    pub(crate) fn key(
        &self,
    ) -> StorageBackendResult<uqa_storage::memory_capture::MemoryCaptureKey<CapturedTableMembership>>
    {
        self.0.key()
    }

    #[cfg(test)]
    pub(crate) fn captured(
        &self,
        cut: &uqa_storage::memory_capture::MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<CapturedTableMembership>>> {
        self.0.captured(cut)
    }
}

pub(crate) struct TableRegistryWrite<'a>(MemoryCaptureWriteGuard<'a, State>);

impl Deref for TableRegistryWrite<'_> {
    type Target = Tables;
    fn deref(&self) -> &Tables {
        &self.0.live
    }
}

impl TableRegistryWrite<'_> {
    pub(crate) fn insert(
        &mut self,
        relation: RelationIdentity,
        table: Arc<TableState>,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if let Some(memory) = &mut self.0.memory {
            let membership = memory.membership.with_table(&relation, &table)?;
            memory
                .publication
                .insert(&relation, &table, membership.sources.get(&relation))?;
            memory.membership = membership;
        }
        Ok(Arc::make_mut(&mut self.0.live).insert(relation, table))
    }

    pub(crate) fn remove(
        &mut self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if let Some(memory) = &mut self.0.memory {
            let membership = memory.membership.without_table(relation)?;
            memory.publication.remove(relation)?;
            memory.membership = membership;
        }
        Ok(Arc::make_mut(&mut self.0.live).remove(relation))
    }

    pub(crate) fn rename(
        &mut self,
        from: &RelationIdentity,
        to: RelationIdentity,
    ) -> StorageBackendResult<Option<Arc<TableState>>> {
        if self.0.live.contains_key(&to) {
            return Ok(None);
        }
        let Some(table) = self.0.live.get(from).cloned() else {
            return Ok(None);
        };
        if let Some(memory) = &mut self.0.memory {
            let membership = memory.membership.with_rename(from, &to)?;
            memory.publication.rename(from, &to);
            memory.membership = membership;
        }
        let live = Arc::make_mut(&mut self.0.live);
        live.remove(from);
        live.insert(to, table.clone());
        Ok(Some(table))
    }

    pub(crate) fn replace(&mut self, live: Tables) -> StorageBackendResult<()> {
        if let Some(memory) = &mut self.0.memory {
            let mut membership = Membership {
                sources: BudgetedSharedMap::new(memory.membership.sources.budget()),
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
        self.0.live = Arc::new(live);
        Ok(())
    }

    pub(crate) fn retain(
        &mut self,
        mut keep: impl FnMut(&RelationIdentity, &Arc<TableState>) -> bool,
    ) -> StorageBackendResult<()> {
        if self.0.memory.is_none() {
            Arc::make_mut(&mut self.0.live).retain(|relation, table| keep(relation, table));
            return Ok(());
        }
        let retained = self
            .0
            .live
            .iter()
            .filter(|(name, table)| keep(name, table))
            .map(|(name, table)| (name.clone(), table.clone()))
            .collect();
        self.replace(retained)
    }
}
