//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve DROP DOMAIN and DROP TYPE targets in source order before joint type and routine removal.

use crate::schema::deletion::CatalogRemovalInputs;
use crate::schema::namespaces::NamespaceCatalogRefresh;
use uqa_sql::{
    ast::{DropKind, DropStmt},
    catalog::dependencies::{ObjectAddress, TYPE_CLASS},
    schema::domains::removal::{
        resolve_drop_domain, resolve_drop_type, BoundTypeDrop, TypeObjectBinding,
    },
    SQLError,
};

pub trait DomainDropNotices {
    fn domain_drop_notice(&self, notice: uqa_sql::SQLNotice);
}
pub struct DomainRemovalContext<'a> {
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub binding: TypeObjectBinding<'a>,
    pub deletion: &'a dyn CatalogRemovalInputs,
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
    let mut originals = Vec::new();
    let resolve = if statement.kind == DropKind::Type {
        resolve_drop_type
    } else {
        resolve_drop_domain
    };
    for name in &statement.names {
        match resolve(&context.binding, name, statement.if_exists)? {
            BoundTypeDrop::Target(oid) => {
                let original = ObjectAddress::whole(TYPE_CLASS, oid);
                if !originals.contains(&original) {
                    originals.push(original);
                }
            }
            BoundTypeDrop::Skipped(message) => context
                .notices
                .domain_drop_notice(uqa_sql::SQLNotice::notice(message)),
        }
    }
    crate::schema::deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |_| Ok(originals.clone()),
        statement.cascade,
    )
}
