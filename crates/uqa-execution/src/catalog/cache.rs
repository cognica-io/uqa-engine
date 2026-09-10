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
    revision: AtomicU64,
}

impl RegtypeOutputCache {
    pub fn is_populated(&self) -> bool {
        self.entry.lock().is_some()
    }

    pub fn get_or_try_init(
        &self,
        build: impl Fn() -> Result<RegtypeOutputCatalog, SQLError>,
    ) -> Result<Arc<RegtypeOutputCatalog>, SQLError> {
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
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.entry.lock().take();
    }
}
