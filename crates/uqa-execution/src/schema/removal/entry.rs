//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dispatch DROP to its namespace, domain, or relation path with the original transaction scope.

use super::RelationRemovalWrite;
use crate::schema::{
    domains::removal::DomainRemovalContext, namespaces::removal::SchemaRemovalContext,
};
use uqa_sql::{
    ast::{DropKind, DropStmt},
    SQLError, SQLResult,
};

pub type SchemaRemovalWrite<'a> =
    Box<dyn FnOnce(&SchemaRemovalContext<'_>) -> Result<SQLResult, SQLError> + 'a>;
pub type DomainRemovalWrite<'a> =
    Box<dyn FnOnce(&DomainRemovalContext<'_>) -> Result<SQLResult, SQLError> + 'a>;

pub trait DropStatementBindings {
    fn with_schema_removal_write(
        &self,
        write: SchemaRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError>;
    fn with_domain_removal_write(
        &self,
        write: DomainRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError>;
    fn with_relation_removal_inputs(
        &self,
        run: RelationRemovalWrite<'_>,
    ) -> Result<SQLResult, SQLError>;
}

pub fn run_drop_statement(
    bindings: &dyn DropStatementBindings,
    statement: DropStmt,
) -> Result<SQLResult, SQLError> {
    match statement.kind {
        DropKind::Schema => bindings.with_schema_removal_write(Box::new(move |context| {
            crate::schema::namespaces::removal::drop_schemas(context, &statement)?;
            Ok(SQLResult::empty())
        })),
        DropKind::Domain => bindings.with_domain_removal_write(Box::new(move |context| {
            crate::schema::domains::removal::drop_domains(context, &statement)?;
            Ok(SQLResult::empty())
        })),
        _ => bindings.with_relation_removal_inputs(Box::new(move |context| {
            super::run_drop(context, statement)
        })),
    }
}
