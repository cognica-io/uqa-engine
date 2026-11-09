//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy-on-write catalog values: snapshots share ownership, never mutable state.

use std::sync::Arc;

use super::CatalogGeneration;

use parking_lot::MappedRwLockReadGuard;
use uqa_core::memory::{Budgeted, Produced, ProductionControl};
use uqa_storage::{
    memory_capture::{
        CapturedMemoryValue, MemoryCaptureCell, MemoryCaptureRegistry, MemoryCaptureSource,
        MemoryCaptureWriteGuard,
    },
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

enum Capture<T, F> {
    Definition(fn(&T, &ProductionControl<'_>) -> Result<Produced<F>, uqa_sql::SQLError>),
    Resource(fn(&T, &StorageReadControl) -> StorageBackendResult<Budgeted<F>>),
}

struct CatalogRoot<T, F> {
    value: CatalogGeneration<T>,
    capture: Option<Capture<T, F>>,
}

impl<T: Send + Sync + 'static, F: Send + Sync + 'static> MemoryCaptureSource for CatalogRoot<T, F> {
    type Frozen = CapturedMemoryValue<F>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let capture = self.capture.as_ref().ok_or_else(|| {
            StorageBackendError::Other("catalog source has no capture producer".into())
        })?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = match capture {
            Capture::Definition(capture) => capture(&self.value, &production)
                .map_err(|error| StorageBackendError::backend("catalog capture", error))?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("catalog capture uses an admitted producer")),
            Capture::Resource(capture) => capture(&self.value, control)?,
        };
        CapturedMemoryValue::new(value, control)
    }
}

pub(crate) struct CatalogCell<T: Send + Sync + 'static, F: Send + Sync + 'static = T>(
    MemoryCaptureCell<CatalogRoot<T, F>>,
);

/// Undo retains the immutable value; a registered source gives that exact generation its own capture identity.
pub(crate) struct CatalogSnapshot<T: Send + Sync + 'static, F: Send + Sync + 'static = T> {
    source: Option<MemoryCaptureCell<CatalogRoot<T, F>>>,
    value: CatalogGeneration<T>,
}

impl<T: Send + Sync + 'static, F: Send + Sync + 'static> std::ops::Deref for CatalogSnapshot<T, F> {
    type Target = CatalogGeneration<T>;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Send + Sync + 'static, F: Send + Sync + 'static> CatalogSnapshot<T, F> {
    pub(crate) fn key(
        &self,
    ) -> StorageBackendResult<uqa_storage::memory_capture::MemoryCaptureKey<CapturedMemoryValue<F>>>
    {
        self.source
            .as_ref()
            .ok_or_else(|| StorageBackendError::Other("snapshot source is not registered".into()))?
            .key()
    }
}

impl<T: Send + Sync + 'static, F: Send + Sync + 'static> CatalogCell<T, F> {
    pub(crate) fn new(value: T) -> Self {
        Self::from_snapshot(Arc::new(value))
    }

    pub(crate) fn from_snapshot(value: impl Into<CatalogGeneration<T>>) -> Self {
        let value = value.into();
        Self(MemoryCaptureCell::new(CatalogRoot {
            value,
            capture: None,
        }))
    }

    pub(crate) fn capture_source(
        value: impl Into<CatalogGeneration<T>>,
        registry: Option<&MemoryCaptureRegistry>,
        capture: fn(&T, &ProductionControl<'_>) -> Result<Produced<F>, uqa_sql::SQLError>,
    ) -> StorageBackendResult<Self> {
        Self::with_source(value.into(), registry, Capture::Definition(capture), None)
    }

    pub(crate) fn capture_source_with_control(
        value: impl Into<CatalogGeneration<T>>,
        registry: &MemoryCaptureRegistry,
        capture: fn(&T, &ProductionControl<'_>) -> Result<Produced<F>, uqa_sql::SQLError>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::with_source(
            value.into(),
            Some(registry),
            Capture::Definition(capture),
            Some(control),
        )
    }

    pub(crate) fn capture_resource(
        value: impl Into<CatalogGeneration<T>>,
        registry: &MemoryCaptureRegistry,
        capture: fn(&T, &StorageReadControl) -> StorageBackendResult<Budgeted<F>>,
    ) -> StorageBackendResult<Self> {
        Self::with_source(
            value.into(),
            Some(registry),
            Capture::Resource(capture),
            None,
        )
    }

    fn with_source(
        value: CatalogGeneration<T>,
        registry: Option<&MemoryCaptureRegistry>,
        capture: Capture<T, F>,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        let mut cell = MemoryCaptureCell::new(CatalogRoot {
            value,
            capture: Some(capture),
        });
        if let Some(registry) = registry {
            if let Some(control) = control {
                cell.register_with_control(registry, control)?;
            } else {
                cell.register(registry)?;
            }
        }
        Ok(Self(cell))
    }

    pub(crate) fn key(
        &self,
    ) -> StorageBackendResult<uqa_storage::memory_capture::MemoryCaptureKey<CapturedMemoryValue<F>>>
    {
        self.0.key()
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, T> {
        parking_lot::RwLockReadGuard::map(self.0.read(), |root| root.value.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    pub(crate) fn snapshot(&self) -> CatalogGeneration<T> {
        self.0.read().value.clone()
    }

    pub(crate) fn source_snapshot(
        &self,
        registry: Option<&MemoryCaptureRegistry>,
    ) -> StorageBackendResult<CatalogSnapshot<T, F>> {
        let root = self.0.read();
        let value = root.value.clone();
        let source = registry
            .map(|registry| {
                let capture = match root.capture.as_ref() {
                    Some(Capture::Definition(copy)) => Capture::Definition(*copy),
                    Some(Capture::Resource(copy)) => Capture::Resource(*copy),
                    None => {
                        return Err(StorageBackendError::Other(
                            "snapshot source has no capture producer".into(),
                        ));
                    }
                };
                registry.cell(CatalogRoot {
                    value: value.clone(),
                    capture: Some(capture),
                })
            })
            .transpose()?;
        Ok(CatalogSnapshot { source, value })
    }

    pub(crate) fn restore(&self, snapshot: &CatalogGeneration<T>) {
        self.0.write().value = snapshot.clone();
    }
}

impl<T: Clone + Send + Sync + 'static, F: Send + Sync + 'static> CatalogCell<T, F> {
    pub(crate) fn write(&self) -> MemoryCaptureWriteGuard<'_, T> {
        MemoryCaptureWriteGuard::map(self.0.write(), |root| {
            CatalogGeneration::make_mut(&mut root.value)
        })
    }
}

impl
    CatalogCell<
        uqa_execution::catalog::foreign::ForeignCatalog,
        uqa_execution::catalog::foreign::registry::CapturedForeignCatalog,
    >
{
    pub(crate) fn tables(
        &self,
    ) -> MappedRwLockReadGuard<'_, uqa_execution::catalog::foreign::ForeignTables> {
        MappedRwLockReadGuard::map(self.read(), |catalog| catalog.tables())
    }
    pub(crate) fn security(
        &self,
    ) -> MappedRwLockReadGuard<'_, uqa_sql::catalog::security::foreign_tables::ForeignTableSecurities>
    {
        MappedRwLockReadGuard::map(self.read(), |catalog| catalog.security())
    }
}

impl
    CatalogCell<
        uqa_execution::catalog::sequence::registry::SequenceCatalog,
        uqa_execution::catalog::sequence::registry::CapturedSequenceCatalog,
    >
{
    pub(crate) fn security(
        &self,
    ) -> MappedRwLockReadGuard<'_, uqa_sql::catalog::security::sequence_registry::SequenceSecurities>
    {
        MappedRwLockReadGuard::map(self.read(), |catalog| catalog.security())
    }
}

#[cfg(test)]
mod tests {
    use super::CatalogCell;
    use std::sync::Arc;

    #[test]
    fn snapshots_share_until_a_writer_detaches() {
        let first = CatalogCell::<Vec<i32>>::new(vec![1]);
        let saved = first.snapshot();
        let second = CatalogCell::<Vec<i32>>::from_snapshot(saved.clone());
        assert!(super::CatalogGeneration::ptr_eq(&saved, &second.snapshot()));
        first.write().push(2);
        assert_eq!(&*second.read(), &[1]);
        assert_eq!(&*first.read(), &[1, 2]);
        assert!(!super::CatalogGeneration::ptr_eq(&first.snapshot(), &saved));
        first.restore(&saved);
        assert!(super::CatalogGeneration::ptr_eq(
            &first.snapshot(),
            &second.snapshot()
        ));
    }

    #[test]
    fn catalog_capture_preserves_the_displaced_root_before_snapshot_restoration() {
        let registry = uqa_storage::memory_capture::MemoryCaptureRegistry::new();
        let original = super::CatalogGeneration::from(Arc::new(vec!["original".to_owned()]));
        let cell = CatalogCell::<Vec<String>>::capture_source(
            original.clone(),
            Some(&registry),
            |value, control| uqa_sql::catalog::definition_capture::names(value, control),
        )
        .unwrap();
        cell.write()[0] = "private".to_owned();
        let key = cell.key().unwrap();
        let control = uqa_storage::read_control::StorageReadControl::with_limit(4096);
        let cut = registry.capture(&control).unwrap();
        assert!(key.retained(&cut).unwrap().is_none());
        cell.restore(&original);
        assert!(super::CatalogGeneration::ptr_eq(
            &cell.snapshot(),
            &original
        ));
        let captured = key.retained(&cut).unwrap().unwrap();
        drop((cell, registry, cut));
        assert_eq!(captured.get().unwrap(), &["private"]);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }

    #[test]
    fn admitted_catalog_generation_survives_snapshot_sources_and_rollback() {
        use uqa_core::memory::RetainedValue;
        use uqa_storage::{
            memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl,
        };

        let registry = MemoryCaptureRegistry::new();
        let input = StorageReadControl::with_limit(4096);
        let generation = RetainedValue::with_budget(7_u64, input.memory()).unwrap();
        let cell =
            CatalogCell::<u64>::capture_source(generation, Some(&registry), |value, control| {
                Ok(control.finish(*value, control.empty_reservation())?)
            })
            .unwrap();
        let saved = cell.source_snapshot(Some(&registry)).unwrap();
        let bytes = input.memory().used();
        assert!(bytes > 0);
        let output = StorageReadControl::with_limit(4096);
        let cut = registry.capture(&output).unwrap();
        input.cancellation().cancel();
        *cell.write() = 8;
        assert_eq!(**saved, 7);
        assert!(!RetainedValue::ptr_eq(&cell.snapshot(), &saved));
        cell.restore(&saved);
        assert!(RetainedValue::ptr_eq(&cell.snapshot(), &saved));
        let captured = saved.key().unwrap().captured(&cut).unwrap();
        drop(cell);
        assert_eq!(input.memory().used(), bytes);
        drop(saved);
        assert_eq!(input.memory().used(), 0);
        assert_eq!(*captured.get().unwrap(), 7);
        drop((cut, captured));
        assert_eq!(output.memory().used(), 0);
    }
}
