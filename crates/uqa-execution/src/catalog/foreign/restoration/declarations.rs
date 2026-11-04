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
    previous: Option<super::super::ForeignCatalog>,
}

impl<'a> ForeignDeclarations<'a> {
    pub(super) fn new(
        context: &ForeignRestoreContext<'a>,
        tables: &BTreeMap<RelationIdentity, StoredForeignTable>,
        security: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    ) -> StorageBackendResult<Self> {
        let previous = context.publication.foreign_catalog();
        let mut next = super::admit_restored(tables, security, previous.budget())?;
        super::merge_session_relations(&previous, &mut next)?;
        context.publication.publish_foreign_catalog(next);
        Ok(Self {
            publication: context.publication,
            previous: Some(previous),
        })
    }
}

impl Drop for ForeignDeclarations<'_> {
    fn drop(&mut self) {
        self.publication.publish_foreign_catalog(
            self.previous
                .take()
                .expect("declarations own their prior generation"),
        );
    }
}
