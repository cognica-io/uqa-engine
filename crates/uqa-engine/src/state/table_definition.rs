//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table expression locks publish SQL-owned immutable generations before changing retained source identities.

use super::{CatalogCell, CatalogGeneration, CatalogSnapshot};
use parking_lot::MappedRwLockReadGuard;
use uqa_sql::catalog::table_definition::{
    SharedTableDefinition, TableDefinition, TableDefinitionValue,
};
use uqa_storage::{
    memory_capture::{
        CapturedMemoryValue, MemoryCaptureKey, MemoryCaptureRegistry, MemoryCaptureWriteGuard,
    },
    StorageBackendResult,
};

pub(crate) struct TableDefinitionCell<T: TableDefinitionValue>(CatalogCell<TableDefinition<T>>);

impl<T: TableDefinitionValue> TableDefinitionCell<T> {
    pub(crate) fn new(value: &T) -> StorageBackendResult<Self> {
        Ok(Self(CatalogCell::new(
            TableDefinition::new(value).map_err(publication_error)?,
        )))
    }

    pub(crate) fn from_generation(value: impl Into<CatalogGeneration<TableDefinition<T>>>) -> Self {
        Self(CatalogCell::from_snapshot(value))
    }

    pub(crate) fn generation(&self) -> CatalogGeneration<TableDefinition<T>> {
        self.0.snapshot()
    }

    pub(crate) fn capture_source(
        self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<Self> {
        Ok(Self(CatalogCell::capture_source(
            self.0.snapshot(),
            Some(registry),
            TableDefinition::capture_with_control,
        )?))
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, T> {
        MappedRwLockReadGuard::map(self.0.read(), |value| &**value)
    }

    pub(crate) fn capture_source_with_control(
        self,
        registry: &MemoryCaptureRegistry,
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self(CatalogCell::capture_source_with_control(
            self.0.snapshot(),
            registry,
            TableDefinition::capture_with_control,
            control,
        )?))
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    pub(crate) fn snapshot(&self) -> SharedTableDefinition<T> {
        self.0.read().shared()
    }

    pub(crate) fn source_snapshot(
        &self,
        registry: Option<&MemoryCaptureRegistry>,
    ) -> StorageBackendResult<CatalogSnapshot<TableDefinition<T>>> {
        self.0.source_snapshot(registry)
    }

    pub(crate) fn restore(&self, snapshot: &CatalogGeneration<TableDefinition<T>>) {
        self.0.restore(snapshot);
    }

    pub(crate) fn key(
        &self,
    ) -> StorageBackendResult<MemoryCaptureKey<CapturedMemoryValue<TableDefinition<T>>>> {
        self.0.key()
    }

    pub(crate) fn write(&self) -> TableDefinitionWrite<'_, T> {
        TableDefinitionWrite(TableDefinition::write(self.0.write()))
    }
}

fn publication_error(error: uqa_sql::SQLError) -> uqa_storage::StorageBackendError {
    uqa_storage::StorageBackendError::backend("table definition publication", error)
}

pub(crate) struct TableDefinitionWrite<'a, T: TableDefinitionValue>(
    uqa_sql::catalog::table_definition::TableDefinitionWrite<
        T,
        MemoryCaptureWriteGuard<'a, TableDefinition<T>>,
    >,
);

impl<T: TableDefinitionValue> TableDefinitionWrite<'_, T> {
    pub(crate) fn publish(&mut self, value: T) -> StorageBackendResult<()> {
        self.0.publish(value).map_err(publication_error)
    }

    pub(crate) fn edit<R>(
        &mut self,
        edit: impl FnOnce(&mut T) -> Result<R, uqa_sql::SQLError>,
    ) -> Result<R, uqa_sql::SQLError> {
        self.0.edit(edit)
    }
}

impl<T: TableDefinitionValue> std::ops::Deref for TableDefinitionWrite<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: TableDefinitionValue> uqa_execution::schema::publication::removal::DefinitionWrite<T>
    for TableDefinitionWrite<'_, T>
{
    fn value(&self) -> &T {
        self
    }
    fn publish(&mut self, value: T) -> StorageBackendResult<()> {
        self.publish(value)
    }
}
