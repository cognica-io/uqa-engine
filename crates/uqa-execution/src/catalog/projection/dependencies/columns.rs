//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The relation columns of one catalog snapshot, for binding the column references of stored statements.

use super::DependencyBuilder;
use uqa_sql::ast::ColumnDef;
use uqa_sql::binding::stored_columns::{StoredColumnBindingContext, StoredColumnCatalog};
use uqa_sql::routines::merge_columns::StoredMergeColumnCatalog;
use uqa_sql::SQLError;

pub(super) struct StoredColumns<'a, 'b> {
    builder: &'a DependencyBuilder<'b>,
}

impl<'a, 'b> StoredColumns<'a, 'b> {
    pub(super) const fn new(builder: &'a DependencyBuilder<'b>) -> Self {
        Self { builder }
    }

    pub(super) fn binding_context(&self) -> StoredColumnBindingContext<'_> {
        StoredColumnBindingContext {
            sources: self,
            merge: self,
        }
    }
}

impl StoredColumnCatalog for StoredColumns<'_, '_> {
    fn stored_relation_column_names(&self, name: &str) -> Result<Option<Vec<String>>, SQLError> {
        crate::catalog::projection::query_source_column_names(self.builder.context, name, true)
    }
}

impl StoredMergeColumnCatalog for StoredColumns<'_, '_> {
    fn stored_merge_target_definitions(&self, table: &str) -> Option<Vec<ColumnDef>> {
        let oid = self.builder.objects.relation_oid_by_name(table)?;
        Some(self.builder.objects.relation(oid)?.columns.clone())
    }
}
