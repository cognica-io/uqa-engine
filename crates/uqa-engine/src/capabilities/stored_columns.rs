//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored column analysis to relation metadata without embedding its AST algorithms.

use crate::Engine;
use uqa_sql::{
    binding::stored_columns::{
        StoredColumnBindingContext, StoredColumnCatalog, StoredSourceCatalog, StoredSourceColumns,
    },
    SQLError,
};

impl StoredColumnCatalog for Engine {
    fn stored_relation_column_names(&self, name: &str) -> Result<Option<Vec<String>>, SQLError> {
        uqa_execution::catalog::projection::query_source_column_names(
            &self.catalog_execution(),
            name,
            true,
        )
    }
}

impl Engine {
    pub(crate) fn stored_column_binding_context(&self) -> StoredColumnBindingContext<'_> {
        StoredColumnBindingContext {
            sources: self,
            merge: self,
        }
    }
}

impl StoredSourceCatalog for Engine {
    fn stored_source_columns(&self) -> StoredSourceColumns {
        let catalog = self.catalog_read_view();
        let mut resolution = self.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::capabilities::RelationLookupMode::Bound);
        StoredSourceColumns {
            catalog: std::sync::Arc::new(catalog),
            resolution,
        }
    }
}
