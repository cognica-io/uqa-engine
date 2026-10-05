//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select catalog definition revisions from one key/value visibility boundary.

use super::{
    relation_key, single_str_key, KeyValueCatalog, RelationKind, StorageBackendError,
    StorageBackendResult, TAG_CATALOG_INDEX, TAG_FOREIGN_TABLE, TAG_METADATA, TAG_SEQUENCE,
    TAG_TABLE, TAG_VIEW,
};
use crate::catalog::{CatalogRecordRef, CatalogRecordRevision};

impl KeyValueCatalog {
    pub(super) fn record_revisions_impl(
        &self,
        records: &[CatalogRecordRef<'_>],
    ) -> StorageBackendResult<Vec<Option<CatalogRecordRevision>>> {
        let mut revisions = None;
        self.store.with_read_view(&mut |read| {
            let mut selected = Vec::with_capacity(records.len());
            for record in records {
                read.control().check()?;
                let key = match record {
                    CatalogRecordRef::Relation(kind, relation) => relation_key(
                        match kind {
                            RelationKind::Table => TAG_TABLE,
                            RelationKind::View => TAG_VIEW,
                            RelationKind::Sequence => TAG_SEQUENCE,
                            RelationKind::ForeignTable => TAG_FOREIGN_TABLE,
                            RelationKind::Index => TAG_CATALOG_INDEX,
                        },
                        relation,
                    )?,
                    CatalogRecordRef::Metadata(key) => single_str_key(TAG_METADATA, key)?,
                };
                selected.push(
                    read.record_revision(&key)?
                        .map(CatalogRecordRevision::KeyValue),
                );
            }
            revisions = Some(selected);
            Ok(())
        })?;
        revisions.ok_or_else(|| {
            StorageBackendError::Other("catalog revision read did not enter its scope".into())
        })
    }
}
