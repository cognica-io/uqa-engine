//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private native membership admits original names, lookup nodes and capture publication together.

use super::{
    Arc, Budgeted, Membership, MemoryBudget, MemoryCaptureCell, MemoryCaptureRegistry, MemoryError,
    MemoryTables, Publication, RelationIdentity, RetainedValue, State, StorageBackendError,
    StorageBackendResult, StorageReadControl, TableRegistry, TableSources, TableState, Tables,
};
use uqa_core::{memory::ProductionControl, RelationName, RelationReference};

pub(super) struct TableRegistryInput {
    live: Tables,
    memory: Option<MemoryTables>,
    control: StorageReadControl,
}

impl TableRegistryInput {
    pub(super) fn new(
        registry: Option<&MemoryCaptureRegistry>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = registry
            .map(|registry| {
                Ok::<_, StorageBackendError>(MemoryTables {
                    membership: Membership::new(control.memory()),
                    publication: Publication::new(registry, control.memory())?,
                })
            })
            .transpose()?;
        Ok(Self {
            live: Tables::new(),
            memory,
            control: control.clone(),
        })
    }

    /// One table's decoded identity enters the ordinary registry used by lookup, undo and capture. Failure consumes every previously reconstructed entry. Persistent membership needs no memory-capture registration.
    pub(super) fn add_table(
        mut self,
        name: Budgeted<RelationIdentity>,
        table: Arc<TableState>,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        let bytes = name
            .schema
            .capacity()
            .checked_add(name.name.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        if !name.budget().shares_allowance(self.control.memory()) || name.reserved_bytes() < bytes {
            return Err(invalid(
                "table identity has incomplete or foreign admission",
            ));
        }
        if self.live.contains_key(&*name)
            || name.schema.is_empty()
            || name.name.is_empty()
            || table.persistence == uqa_sql::ast::RelationPersistence::Temporary
        {
            return Err(invalid(
                "table input has an invalid or repeated permanent identity",
            ));
        }
        self.live.insert_admitted_key_with(name, |name| {
            if let Some(memory) = &mut self.memory {
                let production = ProductionControl::new(
                    self.control.memory(),
                    self.control.cancellation(),
                    self.control.cancellation(),
                );
                let relation = copy_name(name, &production)?;
                let mut source = TableSources::new(&table, &relation)?;
                let (relation, reservation) = relation.into_parts();
                source._name_memory = Some(reservation);
                let membership = memory.membership.with_source(relation, source)?;
                memory.publication.insert_with_control(
                    name,
                    &table,
                    membership.sources.get(name).expect("admitted membership"),
                    &self.control,
                )?;
                memory.membership = membership;
            }
            Ok::<_, StorageBackendError>(table)
        })?;
        Ok(self)
    }

    pub(super) fn finish(self) -> StorageBackendResult<TableRegistry> {
        self.control.check()?;
        let header = self.control.memory().reserve(size_of::<TableRegistry>())?;
        let live = RetainedValue::with_budget(self.live, self.control.memory())?;
        let memory = self.memory.map(|memory| {
            let runtime = MemoryBudget::new(usize::MAX);
            MemoryTables {
                membership: Membership {
                    sources: memory.membership.sources,
                    memory: MemoryBudget::new(usize::MAX),
                    payload_bytes: memory.membership.payload_bytes,
                },
                publication: memory.publication.into_mutation_budget(&runtime),
            }
        });
        let cell = if let Some(memory) = memory {
            let registry = memory.publication.registry().clone();
            registry.cell_with_control(
                State {
                    live,
                    memory: Some(memory),
                },
                &self.control,
            )?
        } else {
            MemoryCaptureCell::new(State { live, memory: None })
        };
        self.control.check()?;
        Ok(TableRegistry {
            cell,
            _image_memory: Some(header),
        })
    }
}

pub(super) fn copy_name(
    value: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<RelationIdentity>> {
    let reference = RelationReference {
        schema: Some(RelationName::decoded(&value.schema)),
        name: RelationName::decoded(&value.name),
    };
    Ok(reference
        .identity_with_control(RelationName::decoded("public"), control)?
        .into_budgeted()
        .expect("table input identity allowance"))
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
