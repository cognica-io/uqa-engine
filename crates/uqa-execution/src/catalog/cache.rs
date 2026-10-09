//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revision-safe publication of catalog output metadata.
use super::projection::RegtypeOutputCatalog;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uqa_sql::SQLError;

#[derive(Default)]
pub struct RegtypeOutputCache {
    entry: Mutex<Option<Arc<RegtypeOutputCatalog>>>,
    initialization: Mutex<()>,
    revision: AtomicU64,
}

impl RegtypeOutputCache {
    /// The session's catalog invalidation generation, including private changes and rollback.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn is_populated(&self) -> bool {
        self.entry.lock().is_some()
    }

    pub fn get_or_try_init(
        &self,
        build: impl Fn() -> Result<RegtypeOutputCatalog, SQLError>,
    ) -> Result<Arc<RegtypeOutputCatalog>, SQLError> {
        if let Some(catalog) = self.entry.lock().clone() {
            return Ok(catalog);
        }
        // Refresh may invalidate output metadata while it is being derived; clear never takes this lock.
        let _initialization = self.initialization.lock();
        loop {
            if let Some(catalog) = self.entry.lock().clone() {
                return Ok(catalog);
            }
            let revision = self.revision.load(Ordering::Acquire);
            let built = Arc::new(build()?);
            let mut cache = self.entry.lock();
            if self.revision.load(Ordering::Acquire) != revision {
                drop(cache);
                continue;
            }
            return Ok(cache.get_or_insert(built).clone());
        }
    }
    pub fn clear(&self) {
        let mut entry = self.entry.lock();
        self.revision.fetch_add(1, Ordering::AcqRel);
        entry.take();
    }
}
