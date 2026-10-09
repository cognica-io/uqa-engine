//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Share immutable dependency derivation without changing DDL's fresh post-lock reads.

use super::CatalogDependencies;
use crate::catalog::{context::CatalogContext, CatalogReadView, RelationNameResolution};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_sql::SQLError;

#[derive(Debug, Default)]
pub(in crate::catalog) struct DependencyCatalogCache {
    entry: Mutex<Option<Arc<CatalogDependencies>>>,
}

impl DependencyCatalogCache {
    pub(in crate::catalog::projection) fn get_or_try_init(
        &self,
        build: impl FnOnce() -> Result<Arc<CatalogDependencies>, SQLError>,
    ) -> Result<Arc<CatalogDependencies>, SQLError> {
        let mut entry = self.entry.lock();
        if let Some(dependencies) = entry.as_ref() {
            return Ok(dependencies.clone());
        }
        let dependencies = build()?;
        *entry = Some(dependencies.clone());
        Ok(dependencies)
    }
}

pub(in crate::catalog::projection) fn retained_dependencies(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Arc<CatalogDependencies>, SQLError> {
    catalog.dependencies.get_or_try_init(|| {
        let metadata = catalog.metadata_view();
        let context = CatalogContext {
            catalog: metadata.as_ref(),
            ..*context
        };
        let mut resolution = resolution.clone();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        CatalogDependencies::build(&context, &metadata, &resolution).map(Arc::new)
    })
}

#[cfg(test)]
thread_local! {
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_build() {
    BUILDS.set(BUILDS.get() + 1);
}

#[cfg(test)]
mod tests;
