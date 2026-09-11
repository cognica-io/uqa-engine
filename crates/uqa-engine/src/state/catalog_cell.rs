//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy-on-write catalog values: snapshots share ownership, never mutable state.

use std::sync::Arc;

use parking_lot::{MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLock};

pub(crate) struct CatalogCell<T>(RwLock<Arc<T>>);

impl<T> CatalogCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(RwLock::new(Arc::new(value)))
    }

    pub(crate) fn from_snapshot(value: Arc<T>) -> Self {
        Self(RwLock::new(value))
    }

    pub(crate) fn read(&self) -> MappedRwLockReadGuard<'_, T> {
        parking_lot::RwLockReadGuard::map(self.0.read(), |value| value.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    pub(crate) fn snapshot(&self) -> Arc<T> {
        Arc::clone(&self.0.read())
    }

    pub(crate) fn restore(&self, snapshot: &Arc<T>) {
        *self.0.write() = Arc::clone(snapshot);
    }
}

impl<T: Clone> CatalogCell<T> {
    pub(crate) fn write(&self) -> MappedRwLockWriteGuard<'_, T> {
        parking_lot::RwLockWriteGuard::map(self.0.write(), Arc::make_mut)
    }
}

#[cfg(test)]
mod tests {
    use super::CatalogCell;
    use std::sync::Arc;

    #[test]
    fn snapshots_share_until_a_writer_detaches() {
        let first = CatalogCell::new(vec![1]);
        let saved = first.snapshot();
        let second = CatalogCell::from_snapshot(Arc::clone(&saved));
        assert!(Arc::ptr_eq(&saved, &second.snapshot()));
        first.write().push(2);
        assert_eq!(&*second.read(), &[1]);
        assert_eq!(&*first.read(), &[1, 2]);
        assert!(!Arc::ptr_eq(&first.snapshot(), &saved));
        first.restore(&saved);
        assert!(Arc::ptr_eq(&first.snapshot(), &second.snapshot()));
    }
}
