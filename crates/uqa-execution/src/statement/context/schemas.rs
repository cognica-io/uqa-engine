//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native schema statement inputs and their catalog transaction boundaries.

use uqa_sql::{SQLError, SQLResult};
pub type SchemaOwnerWrite<'a> = Box<
    dyn FnOnce(&crate::schema::namespaces::SchemaOwnerContext<'_>) -> Result<SQLResult, SQLError>
        + 'a,
>;
pub trait SchemaOwnerTransactions {
    fn with_owner_write(&self, operation: SchemaOwnerWrite<'_>) -> Result<SQLResult, SQLError>;
}
pub trait SchemaStatementInputs<S: Clone + 'static> {
    fn index_creation_context(&self) -> crate::schema::indexes::creation::IndexCreationContext<'_>;
    fn table_alter_entry_context(
        &self,
    ) -> crate::schema::table_alteration::entry::TableAlterEntryContext<'_, S>;
    fn domain_creation_context(&self) -> crate::schema::domains::DomainCreationContext<'_>;
    fn schema_creation_context(&self) -> crate::schema::namespaces::SchemaCreationContext<'_>;
    fn schema_privilege_context(
        &self,
    ) -> crate::schema::namespaces::privileges::SchemaPrivilegeContext<'_>;
    fn database_privilege_context(
        &self,
    ) -> crate::catalog::security::database_lifecycle::DatabasePrivilegeContext<'_>;
    fn sequence_privilege_context(
        &self,
    ) -> crate::catalog::security::sequence_lifecycle::SequencePrivilegeContext<'_>;
    fn vacuum_execution_context(&self) -> crate::maintenance::VacuumContext<'_>;
    fn truncate_context(&self) -> crate::schema::truncate::TruncateContext<'_>;
}
#[derive(Clone)]
pub struct SchemaStatements<'a, S: Clone + 'static> {
    pub creation: &'a dyn crate::schema::table_creation::entry::TableCreationTransactions,
    pub removal: &'a dyn crate::schema::removal::entry::DropStatementBindings,
    pub tables_as: &'a dyn crate::schema::ctas::entry::TableAsTransactions<S>,
    pub views: &'a dyn crate::schema::view_creation::context::ViewCreationTransactions,
    pub view_alteration: &'a dyn crate::schema::view_alteration::ViewAlterTransactions,
    pub foreign_alteration:
        &'a dyn crate::schema::foreign_table_alteration::ForeignTableAlterTransactions,
    pub sequence_creation: &'a dyn crate::schema::sequences::entry::SequenceCreationTransactions,
    pub sequence_alteration: &'a dyn crate::schema::sequences::entry::SequenceAlterTransactions,
    pub owners: &'a dyn SchemaOwnerTransactions,
    pub inputs: &'a dyn SchemaStatementInputs<S>,
}
