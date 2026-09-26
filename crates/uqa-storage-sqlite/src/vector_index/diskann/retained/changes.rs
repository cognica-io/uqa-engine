//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native change identities and canonical tensors share one retained object/field boundary.

use super::{invalid, DiskANNCanonicalOrigin, Family, Identity, RetainedSQLiteDiskANNCanonical};
use crate::mvcc::native::{decode_record, variable_fields_limit};
use rusqlite::types::ValueRef;
use uqa_core::DocId;
use uqa_storage::{
    diskann_index::format::{DiskANNChangeIdentity, CANONICAL_ORIGIN_BYTES, CHANGE_IDENTITY_BYTES},
    mvcc::VersionError,
    read_control::StorageReadControl,
    StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    /// Return each currently journaled document once on this canonical view, including an empty replacement. This does not establish the coverage of a published generation.
    pub fn next_change_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        loop {
            self.check(control)?;
            if after.is_some_and(|document| document >= i64::MAX as DocId) {
                return Ok(None);
            }
            let Some(document) = self.next_changed_document(after, control)? else {
                self.check(control)?;
                return Ok(None);
            };
            after = Some(document);
            let Some(canonical) = self.record(document, control)? else {
                continue;
            };
            let identity = DiskANNChangeIdentity::new(document, canonical.version());
            if self.matches_change(identity, canonical, control)? {
                self.check(control)?;
                return Ok(Some(identity));
            }
        }
    }

    fn next_changed_document(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        let identity = Identity::new(Family::VectorChanges, owner)
            .map_err(VersionError::into_storage_error)?;
        let prefix = identity
            .encode_prefix(&[ValueRef::Text(&self.field)], control)
            .map_err(VersionError::into_storage_error)?;
        let cursor = after
            .map(|document| {
                identity.encode_key(
                    &[
                        ValueRef::Text(&self.field),
                        ValueRef::Blob(&DiskANNChangeIdentity::document_end(document)),
                    ],
                    control,
                )
            })
            .transpose()
            .map_err(VersionError::into_storage_error)?;
        let mut selected = None;
        self.snapshot.view.visit_keys(&prefix, cursor.as_deref(), usize::MAX, control, &mut |key, metadata| {
            self.check(control).map_err(VersionError::Storage)?;
            if !metadata.live {
                return Ok(true);
            }
            let actual = Identity::visit_key_components(key, control, |position, value| {
                match position {
                    0 if value == ValueRef::Text(&self.field) => {}
                    1 => {
                        let bytes = value.as_blob().map_err(|_| VersionError::InvalidEncoding("invalid native change identity"))?;
                        let change = DiskANNChangeIdentity::decode(bytes).map_err(VersionError::Storage)?;
                        if change.document() > i64::MAX as DocId || after.is_some_and(|after| change.document() <= after) {
                            return Err(VersionError::InvalidEncoding("native change cursor did not advance within its document range"));
                        }
                        selected = Some(change.document());
                    }
                    _ => return Err(VersionError::InvalidEncoding("native change key escaped its field")),
                }
                Ok(())
            })?;
            if actual != identity {
                return Err(VersionError::InvalidEncoding("native change key escaped its owner"));
            }
            Ok(false)
        }).map_err(VersionError::into_storage_error)?;
        self.check(control)?;
        Ok(selected)
    }

    fn matches_change(
        &self,
        change: DiskANNChangeIdentity,
        canonical: DiskANNCanonicalOrigin,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let owner = self
            .owner
            .ok_or_else(|| invalid("native change has no canonical owner"))?;
        let identity = Identity::new(Family::VectorChanges, owner)
            .map_err(VersionError::into_storage_error)?;
        let key = identity
            .encode_key(
                &[
                    ValueRef::Text(&self.field),
                    ValueRef::Blob(&change.encode()),
                ],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        let limit = variable_fields_limit(&[
            self.table.len(),
            self.field.len(),
            CHANGE_IDENTITY_BYTES,
            CANONICAL_ORIGIN_BYTES,
        ])
        .map_err(VersionError::into_storage_error)?;
        let mut found = false;
        self.snapshot
            .view
            .visit_value_bounded(&key, limit, control, &mut |record| {
                self.check(control).map_err(VersionError::Storage)?;
                let Some(bytes) = record.and_then(|record| record.value) else {
                    return Ok(());
                };
                let (actual, row) = decode_record(&key, bytes, control)?;
                if actual != identity || row[0] != ValueRef::Text(&self.table) {
                    return Err(VersionError::InvalidEncoding(
                        "native change row scope mismatch",
                    ));
                }
                let bytes = row[3]
                    .as_blob()
                    .map_err(|_| VersionError::InvalidEncoding("invalid native change payload"))?;
                let origin = DiskANNCanonicalOrigin::decode(bytes, self.dimensions)
                    .map_err(VersionError::Storage)?;
                if origin != canonical {
                    return Err(VersionError::InvalidEncoding(
                        "native change differs from its canonical origin",
                    ));
                }
                found = true;
                Ok(())
            })
            .map_err(VersionError::into_storage_error)?;
        self.check(control)?;
        Ok(found)
    }
}
