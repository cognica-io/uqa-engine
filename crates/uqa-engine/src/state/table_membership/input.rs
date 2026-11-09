//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private native membership admits original names, lookup nodes and capture publication together.

use super::*;
use uqa_core::{memory::ProductionControl, RelationName, RelationReference};

pub(crate) struct TableRegistryInput {
    live: Tables,
    membership: Membership,
    publication: Publication,
    control: StorageReadControl,
}

impl TableRegistryInput {
    pub(crate) fn new(
        registry: &MemoryCaptureRegistry,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            live: Tables::new(),
            membership: Membership {
                sources: BudgetedSharedMap::new(control.memory()).snapshot(),
                memory: control.memory().clone(),
                payload_bytes: 0,
            },
            publication: Publication::new(registry, control.memory())?,
            control: control.clone(),
        })
    }

    /// One table's decoded identities enter the same ordinary registry used by runtime lookup, undo and capture. Failure consumes all previously reconstructed membership and its registrations.
    pub(crate) fn add_table(
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
                "table image has an invalid or repeated permanent identity",
            ));
        }
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let relation = copy_name(&name, &production)?;
        let mut source = TableSources::new(&table, &relation)?;
        let (relation, memory) = relation.into_parts();
        source.name_memory = Some(memory);
        let membership = self.membership.with_source(relation, source)?;
        self.publication.insert_with_control(
            &name,
            &table,
            membership.sources.get(&*name).expect("admitted membership"),
            &self.control,
        )?;
        self.live.insert_admitted_key(name, table)?;
        self.membership = membership;
        self.control.check()?;
        Ok(self)
    }

    pub(crate) fn finish(self) -> StorageBackendResult<TableRegistry> {
        self.control.check()?;
        let registry = self.publication.registry().clone();
        let runtime = MemoryBudget::new(usize::MAX);
        let live = RetainedValue::with_budget(self.live, self.control.memory())?;
        let membership = Membership {
            sources: self.membership.sources,
            memory: runtime.clone(),
            payload_bytes: self.membership.payload_bytes,
        };
        let state = State {
            live,
            memory: Some(MemoryTables {
                membership,
                publication: self.publication.into_mutation_budget(&runtime),
            }),
        };
        let value = TableRegistry(registry.cell_with_control(state, &self.control)?);
        self.control.check()?;
        Ok(value)
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
        .expect("table image identity allowance"))
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
