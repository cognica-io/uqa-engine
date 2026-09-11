//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored column analysis to relation metadata without embedding its AST algorithms.

use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{CreateRule, Expr, Statement},
    binding::stored_columns::{self as analysis, StoredColumnBindingContext, StoredColumnCatalog},
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
    pub(crate) fn bind_stored_statement_source_columns(
        &self,
        statement: &mut Statement,
    ) -> Result<bool, SQLError> {
        let catalog = self.catalog_read_view();
        let mut resolution = self.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::capabilities::RelationLookupMode::Bound);
        analysis::bind_stored_statement_source_columns(statement, |name| {
            Ok(
                if let Some(table) = catalog.table_resolved(&resolution, name)? {
                    Some(
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect(),
                    )
                } else {
                    catalog
                        .foreign_table_resolved(&resolution, name)?
                        .map(|table| {
                            table
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        })
                },
            )
        })
    }
    pub(crate) fn bind_rule_condition_column_dependencies(
        &self,
        condition: &mut Expr,
    ) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
        analysis::bind_rule_condition_column_dependencies(
            self.stored_column_binding_context(),
            condition,
        )
    }
    pub(crate) fn bind_rule_action_column_dependencies(
        &self,
        action: &mut Statement,
    ) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
        analysis::bind_rule_action_column_dependencies(self.stored_column_binding_context(), action)
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
