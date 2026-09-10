//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind constraint execution to the active catalog, row generation, and transaction.
use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::{DocId, PostingList, Predicate, Value};
use uqa_execution::catalog::security::schema::SchemaAclPrivilege;
use uqa_execution::{
    mutation::constraints::context::{
        ConstraintCatalog, ConstraintContext, ConstraintTransactions, MutationIndexRead,
        MutationNamespace, MutationRead,
    },
    row_locks::LockAcquire,
};
use uqa_sql::{
    ast::{ColumnDef, ColumnType, ForeignKey, TableCheck},
    catalog::index::EnforcedKey,
    SQLError,
};
use uqa_storage::{document_store::Document, ValueIndexKey};
impl Engine {
    pub(crate) fn constraint_execution_context(&self) -> ConstraintContext<'_> {
        ConstraintContext {
            catalog: self,
            reads: self,
            indexes: self,
            transactions: self,
            locks: self,
            namespace: self,
            referrers: self,
            partitions: self.partition_context(),
        }
    }
}
impl ConstraintCatalog for Engine {
    fn try_unique_columns(&self, table: &str) -> Result<Vec<String>, String> {
        Engine::try_unique_columns(self, table).map_err(|error| error.to_string())
    }
    fn try_check_constraint_definitions(&self, table: &str) -> Result<Vec<TableCheck>, String> {
        Engine::try_check_constraint_definitions(self, table).map_err(|error| error.to_string())
    }
    fn try_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, String> {
        Engine::try_foreign_keys(self, table).map_err(|error| error.to_string())
    }
    fn column_type(&self, table: &str, column: &str) -> Result<Option<ColumnType>, String> {
        Engine::column_type(self, table, column).map_err(|error| error.to_string())
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_scan_tables(self, table, descendants)
    }
}
impl MutationRead for Engine {
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        Engine::table_doc_ids(self, table)
    }
    fn live_table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        Engine::live_table_doc_ids(self, table)
    }
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        Engine::get_document(self, table, doc_id)
    }
    fn command_overlay_changed_ids(
        &self,
        table: &str,
    ) -> Result<Option<BTreeSet<DocId>>, SQLError> {
        self.command_overlay_changes(table)
            .map(|changes| changes.map(|changes| changes.into_keys().collect()))
    }
}
impl MutationIndexRead for Engine {
    fn find_conflict(
        &self,
        table: &str,
        columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        Engine::find_conflict(self, table, columns, values)
    }
    fn value_index_scan_key(
        &self,
        table: &str,
        key: &ValueIndexKey,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        Engine::value_index_scan_key(self, table, key, predicate)
    }
}
impl ConstraintTransactions for Engine {
    fn foreign_key_is_deferred(&self, table: &str, key: &ForeignKey) -> Result<bool, SQLError> {
        Engine::foreign_key_is_deferred(self, table, key)
    }
    fn refresh_explicit_statement_snapshot(&self) -> Result<(), SQLError> {
        Engine::refresh_explicit_statement_snapshot(self)
    }
    fn lock_key_reservation(&self, key: [u8; 32], table: &str) -> Result<LockAcquire, SQLError> {
        Engine::lock_key_reservation(self, key, table)
    }
}
impl MutationNamespace for Engine {
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn require_schema_privilege(
        &self,
        schema: &str,
        role: &str,
        privilege: SchemaAclPrivilege,
    ) -> Result<(), SQLError> {
        Engine::require_schema_privilege(self, schema, role, privilege)
    }
}

impl uqa_sql::semantics::conflict::ConflictCatalog for Engine {
    fn try_describe_table(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table(self, table).map_err(|error| error.to_string())
    }
    fn enforced_keys(&self, table: &str) -> Result<Vec<EnforcedKey>, String> {
        Engine::enforced_keys(self, table).map_err(|error| error.to_string())
    }
    fn try_declared_table_constraints(
        &self,
        table: &str,
    ) -> Result<uqa_sql::ast::TableConstraintSet, String> {
        Engine::try_declared_table_constraints(self, table).map_err(|error| error.to_string())
    }
}
impl uqa_sql::semantics::conflict::InferenceBindingScope for Engine {
    fn binding_scope(&self) -> Result<uqa_sql::binding::snapshot::BindingSnapshot, SQLError> {
        let scope = super::query_scope::new_for_catalog_binding(self);
        uqa_execution::query::binding::binding_context(&scope)
            .map(uqa_sql::binding::snapshot::BindingSnapshot::from)
    }
}
impl Engine {
    pub(crate) fn inference_context(&self) -> uqa_sql::semantics::conflict::InferenceContext<'_> {
        uqa_sql::semantics::conflict::InferenceContext {
            catalog: self,
            aggregates: self,
            routines: self,
            binding: self,
        }
    }
}

impl Engine {
    pub(crate) fn index_predicate_accepts(
        &self,
        table: &str,
        predicate: Option<&uqa_sql::ast::Expr>,
        document: &Document,
    ) -> Result<bool, SQLError> {
        uqa_execution::mutation::constraints::index_keys::index_predicate_accepts(
            self.constraint_execution_context().index_expressions(),
            table,
            predicate,
            document,
        )
    }

    pub(crate) fn index_key_values(
        &self,
        table: &str,
        keys: &[uqa_sql::ast::IndexKey],
        document: &Document,
    ) -> Result<Vec<Value>, SQLError> {
        uqa_execution::mutation::constraints::index_keys::index_key_values(
            self.constraint_execution_context().index_expressions(),
            table,
            keys,
            document,
        )
    }

    pub(crate) fn validate_deferred_foreign_key_checks(
        &self,
        checks: &[crate::DeferredForeignKeyCheck],
        targets: Option<&std::collections::BTreeSet<crate::ConstraintIdentity>>,
    ) -> Result<(), SQLError> {
        uqa_execution::mutation::constraints::validate_deferred_foreign_key_checks(
            self.constraint_execution_context(),
            checks,
            targets,
        )
    }
}
