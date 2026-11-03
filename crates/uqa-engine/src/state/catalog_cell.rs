//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy-on-write catalog values: snapshots share ownership, never mutable state.

use std::sync::Arc;

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
    value: Arc<T>,
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

impl<T: Send + Sync + 'static, F: Send + Sync + 'static> CatalogCell<T, F> {
    pub(crate) fn new(value: T) -> Self {
        Self::from_snapshot(Arc::new(value))
    }

    pub(crate) fn from_snapshot(value: Arc<T>) -> Self {
        Self(MemoryCaptureCell::new(CatalogRoot {
            value,
            capture: None,
        }))
    }

    pub(crate) fn capture_source(
        value: Arc<T>,
        registry: Option<&MemoryCaptureRegistry>,
        capture: fn(&T, &ProductionControl<'_>) -> Result<Produced<F>, uqa_sql::SQLError>,
    ) -> StorageBackendResult<Self> {
        Self::with_source(value, registry, Capture::Definition(capture))
    }

    pub(crate) fn capture_resource(
        value: Arc<T>,
        registry: &MemoryCaptureRegistry,
        capture: fn(&T, &StorageReadControl) -> StorageBackendResult<Budgeted<F>>,
    ) -> StorageBackendResult<Self> {
        Self::with_source(value, Some(registry), Capture::Resource(capture))
    }

    fn with_source(
        value: Arc<T>,
        registry: Option<&MemoryCaptureRegistry>,
        capture: Capture<T, F>,
    ) -> StorageBackendResult<Self> {
        let mut cell = MemoryCaptureCell::new(CatalogRoot {
            value,
            capture: Some(capture),
        });
        if let Some(registry) = registry {
            cell.register(registry)?;
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

    pub(crate) fn snapshot(&self) -> Arc<T> {
        Arc::clone(&self.0.read().value)
    }

    pub(crate) fn restore(&self, snapshot: &Arc<T>) {
        self.0.write().value = Arc::clone(snapshot);
    }
}

impl<T: Clone + Send + Sync + 'static, F: Send + Sync + 'static> CatalogCell<T, F> {
    pub(crate) fn write(&self) -> MemoryCaptureWriteGuard<'_, T> {
        MemoryCaptureWriteGuard::map(self.0.write(), |root| Arc::make_mut(&mut root.value))
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
        let second = CatalogCell::<Vec<i32>>::from_snapshot(Arc::clone(&saved));
        assert!(Arc::ptr_eq(&saved, &second.snapshot()));
        first.write().push(2);
        assert_eq!(&*second.read(), &[1]);
        assert_eq!(&*first.read(), &[1, 2]);
        assert!(!Arc::ptr_eq(&first.snapshot(), &saved));
        first.restore(&saved);
        assert!(Arc::ptr_eq(&first.snapshot(), &second.snapshot()));
    }

    #[test]
    fn catalog_capture_preserves_the_displaced_root_before_snapshot_restoration() {
        let registry = uqa_storage::memory_capture::MemoryCaptureRegistry::new();
        let original = Arc::new(vec!["original".to_owned()]);
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
        assert!(Arc::ptr_eq(&cell.snapshot(), &original));
        let captured = key.retained(&cut).unwrap().unwrap();
        drop((cell, registry, cut));
        assert_eq!(captured.get().unwrap(), &["private"]);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}
