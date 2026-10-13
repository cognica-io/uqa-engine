//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore expression inputs against every retained foreign relation's actual row descriptor.

use super::{BoundTableSecurity, ForeignRestoreContext};
use crate::catalog::foreign::StoredForeignTable;
use crate::schema::foreign_table_alteration::ForeignTableAlterPublication;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_storage::StorageBackendResult;

/// The previous registry returns on both success and error; the caller publishes the complete, validated catalog afterward.
pub(super) struct ForeignDeclarations<'a> {
    publication: &'a dyn ForeignTableAlterPublication,
    previous: BTreeMap<RelationIdentity, StoredForeignTable>,
    security: BTreeMap<RelationIdentity, BoundTableSecurity>,
}

impl<'a> ForeignDeclarations<'a> {
    pub(super) fn new(
        context: &ForeignRestoreContext<'a>,
        tables: &BTreeMap<RelationIdentity, StoredForeignTable>,
        security: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    ) -> StorageBackendResult<Self> {
        let mut tables = tables.clone();
        let mut security = security.clone();
        super::merge_session_relations(
            &context.registry.tables(),
            &context.registry.security(),
            &mut tables,
            &mut security,
        )?;
        let previous = std::mem::replace(&mut **context.publication.tables_write(), tables);
        let security = std::mem::replace(&mut **context.publication.security_write(), security);
        Ok(Self {
            publication: context.publication,
            previous,
            security,
        })
    }
}

impl Drop for ForeignDeclarations<'_> {
    fn drop(&mut self) {
        **self.publication.tables_write() = std::mem::take(&mut self.previous);
        **self.publication.security_write() = std::mem::take(&mut self.security);
    }
}
