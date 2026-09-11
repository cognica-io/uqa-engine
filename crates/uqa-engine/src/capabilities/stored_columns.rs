//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored column analysis to relation metadata without embedding its AST algorithms.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::CreateRule,
    binding::stored_columns::{
        self as analysis, StoredColumnBindingContext, StoredColumnCatalog, StoredSourceCatalog,
        StoredSourceColumns,
    },
    catalog::events::RuleColumnDependency,
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
    pub(crate) fn rewrite_rule_column_references(
        &self,
        definition: &mut CreateRule,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        analysis::rewrite_rule_column_references(
            self.stored_column_binding_context(),
            definition,
            relation,
            from,
            to,
        )
    }
    pub(crate) fn remove_rule_source_column_aliases(
        &self,
        definition: &mut CreateRule,
        dependency: &RuleColumnDependency,
    ) -> Result<bool, SQLError> {
        analysis::remove_rule_source_column_aliases(
            self.stored_column_binding_context(),
            definition,
            dependency,
        )
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
