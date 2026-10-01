//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native ACL tuple records share their relation definition's snapshot and atomic batch.

use super::{string, text, Family, NativeRecordOwner, NativeSnapshot, Result};
use uqa_storage::{catalog::relation_acl, KeyValueBatch, RelationIdentity};

impl NativeSnapshot {
    fn visit_relation_acls(
        &self,
        relation: &RelationIdentity,
        mut visitor: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        let prefix = relation_acl::prefix(relation);
        self.visit_text_prefix_rows(
            Family::Metadata,
            NativeRecordOwner::Database(self.database),
            &prefix,
            |row| {
                let key = string(row[0])?;
                visitor(&key, &string(row[1])?)
            },
        )
    }

    pub(in crate::catalog) fn load_relation_acls(
        &self,
    ) -> Result<relation_acl::RelationAclRecords> {
        let mut records = relation_acl::RelationAclRecords::default();
        self.visit_text_prefix_rows(
            Family::Metadata,
            NativeRecordOwner::Database(self.database),
            relation_acl::METADATA_PREFIX,
            |row| {
                let key = string(row[0])?;
                records.insert(&key, string(row[1])?.as_bytes())?;
                Ok(())
            },
        )?;
        Ok(records)
    }

    pub(in crate::catalog) fn clear_relation_acls(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
    ) -> Result<()> {
        self.visit_relation_acls(relation, |key, _| {
            self.delete_prefix(
                batch,
                Family::Metadata,
                NativeRecordOwner::Database(self.database),
                &[text(key)],
            )
        })
    }

    pub(in crate::catalog) fn rename_relation_acls(
        &self,
        batch: &mut dyn KeyValueBatch,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<()> {
        let prefix = relation_acl::prefix(from);
        let owner = NativeRecordOwner::Database(self.database);
        self.visit_relation_acls(from, |key, value| {
            let column = relation_acl::column(&prefix, key)?;
            let next = relation_acl::key(to, column.as_deref());
            self.put_row(batch, Family::Metadata, owner, &[text(&next), text(value)])?;
            self.delete_prefix(batch, Family::Metadata, owner, &[text(key)])
        })
    }
}
