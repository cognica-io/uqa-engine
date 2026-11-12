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

impl KeyConstraintNames {
    /// Load the version and original index rows through bounded provider reads. Legacy catalogs skip index enumeration; complete stored definitions still precede current-name selection. The caller retains the surrounding catalog transaction.
    pub async fn load_from(
        catalog: &dyn uqa_storage::CatalogFacade,
        limits: uqa_storage::catalog::input::CatalogInputLimits,
        control: &uqa_storage::read_control::StorageReadControl,
        reader: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        reader.checkpoint().await?;
        let version =
            catalog.get_metadata_with_control(super::REGISTRY_VERSION, limits, control)?;
        let mut input = KeyConstraintNameInput::new(
            version.as_ref().map(|version| version.as_str()),
            control.memory(),
            control.cancellation(),
            reader.cancellation,
        )?;
        drop(version);
        let State::Reading(state) = &input.state else {
            unreachable!("new key-name input")
        };
        if state.names.names.is_some() {
            let mut rows = catalog.begin_index_input(limits, control)?;
            loop {
                control.check()?;
                reader.checkpoint().await?;
                match rows.advance()? {
                    uqa_storage::catalog::input::CatalogInputStep::Pending => (),
                    uqa_storage::catalog::input::CatalogInputStep::Row(row) => input.push(&row)?,
                    uqa_storage::catalog::input::CatalogInputStep::Complete => break,
                }
            }
        }
        input.finish()
    }
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
