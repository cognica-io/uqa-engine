//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental index-owned names retain their original allocation and cancellation scopes.

use super::{invalid, registry_owns_names, KeyConstraintNames, KeyName};
use uqa_core::{
    memory::{MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity, RelationName,
};
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub struct KeyConstraintNameInput {
    state: State,
}

enum State {
    Reading(Input),
    Failed,
}

struct Input {
    names: KeyConstraintNames,
    memory: MemoryBudget,
    original: CancellationToken,
    invoking: CancellationToken,
}

impl KeyConstraintNameInput {
    /// Retain the original controls without enumerating indexes. The caller supplies version and rows from its same retained catalog view; legacy versions do not inspect index definitions.
    pub fn new(
        version: Option<&str>,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> StorageBackendResult<Self> {
        ProductionControl::new(memory, original, invoking).check()?;
        let names = registry_owns_names(version)?.then(OwnedMap::new);
        Ok(Self {
            state: State::Reading(Input {
                names: KeyConstraintNames { names },
                memory: memory.clone(),
                original: original.clone(),
                invoking: invoking.clone(),
            }),
        })
    }

    /// Consume one already admitted native row. Complete definition validation precedes ownership lookup, including for unrelated indexes. Failure or unwinding retires every earlier name and permanently closes the input.
    pub fn push(&mut self, row: &CatalogIndexRow) -> StorageBackendResult<()> {
        let State::Reading(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(invalid("key constraint name input is finished"));
        };
        input.push(row)?;
        self.state = State::Reading(input);
        Ok(())
    }

    /// Transfer the completed owner map after the retained row source reaches EOF. The map keeps each original node and name reservation independently of this input and its source rows.
    pub fn finish(self) -> StorageBackendResult<KeyConstraintNames> {
        let State::Reading(input) = self.state else {
            return Err(invalid("key constraint name input is finished"));
        };
        ProductionControl::new(&input.memory, &input.original, &input.invoking).check()?;
        Ok(input.names)
    }
}

impl Input {
    fn push(&mut self, row: &CatalogIndexRow) -> StorageBackendResult<()> {
        let control = ProductionControl::new(&self.memory, &self.original, &self.invoking);
        control.check()?;
        if self.names.names.is_none() {
            return Ok(());
        }
        let definition = uqa_sql::catalog::index::stored::input::definition(
            row.definition_json.as_deref(),
            &self.memory,
            &self.original,
            &self.invoking,
        )
        .map_err(sql_error)?;
        let Some(owner) = definition.relationships.owning_constraint else {
            return Ok(());
        };
        let identity = definition
            .catalog
            .as_ref()
            .ok_or_else(|| invalid("owned index has no catalog identity"))?;
        let reference = RelationIdentity::try_borrowed_reference(&row.table_name)
            .map_err(|_| invalid("invalid key constraint table name"))?;
        let table = reference.identity_with_control(RelationName::decoded("public"), &control)?;
        let name = control.copy_text(&row.relation.name)?;
        let (table, table_memory) = table.into_parts();
        let (name, name_memory) = name.into_parts();
        self.names.insert(
            owner,
            &row.relation.schema,
            KeyName {
                table,
                table_object_id: identity.table_object_id,
                name,
                _memory: control.combine(table_memory, name_memory),
            },
            &control,
        )?;
        control.check()?;
        Ok(())
    }
}

pub(super) fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("stored key constraint names", error),
    }
}
