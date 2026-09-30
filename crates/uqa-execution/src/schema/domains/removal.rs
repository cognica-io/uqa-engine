//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve DROP DOMAIN and DROP TYPE targets in source order before joint type and routine removal.

use crate::schema::namespaces::NamespaceCatalogRefresh;
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{DropKind, DropStmt},
    schema::domains::removal::{
        resolve_drop_domain, resolve_drop_type, BoundTypeDrop, TypeObjectBinding,
    },
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
    pub binding: TypeObjectBinding<'a>,
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
    let resolve = if statement.kind == DropKind::Type {
        resolve_drop_type
    } else {
        resolve_drop_domain
    };
    for name in &statement.names {
        match resolve(&context.binding, name, statement.if_exists)? {
            BoundTypeDrop::Target(oid) => {
                targets.insert(oid);
            }
            BoundTypeDrop::Skipped(message) => context.notices.domain_drop_notice(&message),
        }
    }
    context
        .removal
        .remove_domain_types_and_routines(&targets, statement.cascade)
}
