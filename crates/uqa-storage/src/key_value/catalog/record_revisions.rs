//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select catalog definition revisions from one key/value visibility boundary.

use super::{
    KeyValueCatalog, RelationKind, StorageBackendError, StorageBackendResult, TAG_CATALOG_INDEX,
    TAG_FOREIGN_TABLE, TAG_METADATA, TAG_SEQUENCE, TAG_TABLE, TAG_VIEW,
};
use crate::catalog::{CatalogRecordRef, CatalogRecordRevision};

impl KeyValueCatalog {
    pub(super) fn record_revisions_impl(
        &self,
        records: &[CatalogRecordRef<'_>],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> StorageBackendResult<uqa_core::memory::Produced<Vec<Option<CatalogRecordRevision>>>> {
        let mut revisions = None;
        self.store.with_read_view(&mut |read| {
            let mut selected = uqa_core::memory::ProductionVec::new(*control);
            selected.reserve(records.len())?;
            for record in records {
                read.control().check()?;
                let key = match record {
                    CatalogRecordRef::Relation(kind, relation) => {
                        crate::key_value::codec::string_key_with_control(
                            match kind {
                                RelationKind::Table => TAG_TABLE,
                                RelationKind::View => TAG_VIEW,
                                RelationKind::Sequence => TAG_SEQUENCE,
                                RelationKind::ForeignTable => TAG_FOREIGN_TABLE,
                                RelationKind::Index => TAG_CATALOG_INDEX,
                            },
                            [relation.schema.as_str(), relation.name.as_str()],
                            control,
                        )?
                    }
                    CatalogRecordRef::Metadata(key) => {
                        crate::key_value::codec::string_key_with_control(
                            TAG_METADATA,
                            [*key],
                            control,
                        )?
                    }
                };
                selected.push_produced(
                    control.finish(
                        read.record_revision(&key)?
                            .map(CatalogRecordRevision::KeyValue),
                        control.empty_reservation(),
                    )?,
                )?;
            }
            revisions = Some(selected.finish()?);
            Ok(())
        })?;
        revisions.ok_or_else(|| {
            StorageBackendError::Other("catalog revision read did not enter its scope".into())
        })
    }
}
