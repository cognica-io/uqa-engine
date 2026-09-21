//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime catalog functions keep the original participant through snapshot refresh.

use super::{CatalogContext, CatalogReadView, CatalogSnapshotSource, SQLError};

struct QueryCatalogSource<'a> {
    source: &'a dyn CatalogSnapshotSource,
    original: CatalogReadView,
}

impl QueryCatalogSource<'_> {
    fn bind(&self, mut snapshot: CatalogReadView) -> CatalogReadView {
        snapshot.graph_reads.clone_from(&self.original.graph_reads);
        snapshot
    }
}

impl CatalogSnapshotSource for QueryCatalogSource<'_> {
    fn catalog_snapshot(&self) -> CatalogReadView {
        self.bind(self.source.catalog_snapshot())
    }

    fn refreshed_catalog_snapshot(&self) -> Result<CatalogReadView, SQLError> {
        self.source
            .refreshed_catalog_snapshot()
            .map(|view| self.bind(view))
    }

    fn current_catalog_snapshot(&self) -> CatalogReadView {
        self.source.current_catalog_snapshot()
    }

    fn bind_query_reads(&self, snapshot: CatalogReadView) -> Result<CatalogReadView, SQLError> {
        Ok(self.bind(snapshot))
    }
}

impl CatalogContext<'_> {
    /// Bind actual scalar catalog consumption without changing binding, restoration or snapshot-refresh semantics.
    pub fn with_query_reads<T>(
        &self,
        read: impl FnOnce(&CatalogContext<'_>) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let source = QueryCatalogSource {
            source: self.catalog,
            original: self
                .catalog
                .bind_query_reads(self.catalog.catalog_snapshot())?,
        };
        let context = CatalogContext {
            catalog: &source,
            ..*self
        };
        read(&context)
    }
}

impl CatalogReadView {
    pub(in crate::catalog) fn without_query_reads(mut self) -> Self {
        self.graph_reads = None;
        self
    }
}
