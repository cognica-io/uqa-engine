//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ACL tuple composition and lifecycle share the definition's evaluated Key/Value view.

use super::{read_str, single_str_key, TAG_METADATA};
use crate::{
    catalog::relation_acl, key_value::KeyValueRead, KeyValueBatch, RelationIdentity,
    StorageBackendResult,
};

pub(super) fn visit(
    read: &dyn KeyValueRead,
    relation: &RelationIdentity,
    visitor: &mut impl FnMut(&[u8], &str, &[u8]) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let prefix = relation_acl::prefix(relation);
    read.visit_prefix(&[TAG_METADATA], &mut |key, value| {
        let name = read_str(key, &mut 1)?;
        if name.starts_with(&prefix) {
            visitor(key, &name, value)?;
        }
        Ok(())
    })
}

pub(super) fn load(
    read: &dyn KeyValueRead,
) -> StorageBackendResult<relation_acl::RelationAclRecords> {
    let mut records = relation_acl::RelationAclRecords::default();
    read.visit_prefix(&[TAG_METADATA], &mut |key, value| {
        let name = read_str(key, &mut 1)?;
        if name.starts_with(relation_acl::METADATA_PREFIX) {
            records.insert(&name, value)?;
        }
        Ok(())
    })?;
    Ok(records)
}

pub(super) fn clear(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    relation: &RelationIdentity,
) -> StorageBackendResult<()> {
    visit(read, relation, &mut |key, _, _| batch.delete(key))
}

pub(super) fn rename(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> StorageBackendResult<()> {
    let prefix = relation_acl::prefix(from);
    visit(read, from, &mut |key, name, value| {
        let column = relation_acl::column(&prefix, name)?;
        batch.put(
            &single_str_key(TAG_METADATA, &relation_acl::key(to, column.as_deref()))?,
            value,
        )?;
        batch.delete(key)
    })
}
