//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table metadata locks adapt SQL's native immutable generations to retained source identities.

use super::{CatalogCell, CatalogSnapshot};
use parking_lot::MappedRwLockReadGuard;
use std::sync::Arc;
use uqa_sql::catalog::table_metadata::{
    SharedTableMetadata, TableMetadata, TableMetadataValue, TableMetadataWrite,
};
use uqa_storage::{
    memory_capture::{
        CapturedMemoryValue, MemoryCaptureKey, MemoryCaptureRegistry, MemoryCaptureWriteGuard,
    },
    StorageBackendResult,
};

pub(crate) struct TableMetadataCell<
    T: TableMetadataValue,
    F: Send + Sync + 'static = TableMetadata<T>,
>(CatalogCell<TableMetadata<T>, F>);

impl<T: TableMetadataValue, F: Send + Sync + 'static> TableMetadataCell<T, F> {
    pub(crate) fn new(value: T) -> Self {
        Self(CatalogCell::new(TableMetadata::new(value)))
    }

    pub(crate) fn from_generation(value: Arc<TableMetadata<T>>) -> Self {
        Self(CatalogCell::from_snapshot(value))
    }

    pub(crate) fn generation(&self) -> Arc<TableMetadata<T>> {
        self.0.snapshot()
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, T> {
        MappedRwLockReadGuard::map(self.0.read(), |value| &**value)
    }

    pub(crate) fn snapshot(&self) -> SharedTableMetadata<T> {
        self.0.read().snapshot()
    }

    pub(crate) fn source_snapshot(
        &self,
        registry: Option<&MemoryCaptureRegistry>,
    ) -> StorageBackendResult<CatalogSnapshot<TableMetadata<T>, F>> {
        self.0.source_snapshot(registry)
    }

    pub(crate) fn restore(&self, snapshot: &Arc<TableMetadata<T>>) {
        self.0.restore(snapshot);
    }

    pub(crate) fn key(&self) -> StorageBackendResult<MemoryCaptureKey<CapturedMemoryValue<F>>> {
        self.0.key()
    }

    pub(crate) fn write(
        &self,
    ) -> TableMetadataWrite<T, MemoryCaptureWriteGuard<'_, TableMetadata<T>>> {
        TableMetadata::write(self.0.write())
    }
}

impl<T: TableMetadataValue> TableMetadataCell<T> {
    pub(crate) fn capture_source(
        self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<Self> {
        Ok(Self(CatalogCell::capture_source(
            self.0.snapshot(),
            Some(registry),
            TableMetadata::capture_with_control,
        )?))
    }
}

impl
    TableMetadataCell<
        uqa_sql::catalog::security::BoundTableSecurity,
        uqa_sql::catalog::security::table_binding::CapturedTableSecurity,
    >
{
    pub(crate) fn capture_source(
        self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<Self> {
        Ok(Self(CatalogCell::capture_source(
            self.0.snapshot(),
            Some(registry),
            |value, control| value.capture_security(control),
        )?))
    }
}
