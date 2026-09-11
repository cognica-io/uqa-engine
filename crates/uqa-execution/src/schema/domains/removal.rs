//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve DROP DOMAIN targets in source order before joint domain and routine removal.

use crate::schema::namespaces::NamespaceCatalogRefresh;
use std::collections::BTreeSet;
use uqa_sql::{
    ast::DropStmt,
    schema::domains::removal::{resolve_drop_domain, BoundDomainDrop, DomainDropBinding},
    SQLError,
};

pub trait DomainRoutineRemoval {
    fn remove_domain_types_and_routines(
        &self,
        targets: &BTreeSet<u32>,
        cascade: bool,
    ) -> Result<(), SQLError>;
}
pub trait DomainDropNotices {
    fn domain_drop_notice(&self, message: &str);
}
pub struct DomainRemovalContext<'a> {
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub binding: DomainDropBinding<'a>,
    pub removal: &'a dyn DomainRoutineRemoval,
    pub notices: &'a dyn DomainDropNotices,
}

pub fn drop_domains(
    context: &DomainRemovalContext<'_>,
    statement: &DropStmt,
) -> Result<(), SQLError> {
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let mut targets = BTreeSet::new();
    for name in &statement.names {
        match resolve_drop_domain(&context.binding, name, statement.if_exists)? {
            BoundDomainDrop::Target(oid) => {
                targets.insert(oid);
            }
            BoundDomainDrop::Skipped(message) => context.notices.domain_drop_notice(&message),
        }
    }
    context
        .removal
        .remove_domain_types_and_routines(&targets, statement.cascade)
}
