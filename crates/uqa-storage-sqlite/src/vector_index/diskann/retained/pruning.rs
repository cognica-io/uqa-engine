//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, DiskANNCanonicalOrigin, Family, Identity, NativeSnapshot,
    RetainedSQLiteDiskANNCanonical,
};
use crate::mvcc::native::{decode_record, variable_fields_limit};
use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    diskann_index::{
        catalog::DiskANNIndexResolver,
        changes::{DiskANNChangeJournal, DiskANNPruneRequest, DiskANNPruneResult},
        format::{DiskANNChangeIdentity, CANONICAL_ORIGIN_BYTES, CHANGE_IDENTITY_BYTES},
    },
    key_value::{KeyValueDiskANNPruner, KeyValueRead},
    mvcc::VersionError,
    read_control::StorageReadControl,
    KeyValueBatch, StorageBackendResult,
};

impl RetainedSQLiteDiskANNCanonical {
    pub(crate) fn prune_changes(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        pruner: &KeyValueDiskANNPruner,
        mutation: (&NativeSnapshot, &mut dyn KeyValueBatch),
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        let (current, batch) = mutation;
        let read = current.record_read();
        self.require_current_index(&read, batch, control)?;
        let owner = self
            .owner
            .ok_or_else(|| invalid("pruning has no canonical owner"))?;
        let prefix = |family| {
            Identity::new(family, owner)
                .and_then(|identity| {
                    identity.encode_prefix(&[ValueRef::Text(&self.field)], control)
                })
                .map_err(VersionError::into_storage_error)
        };
        let vectors = prefix(Family::Vectors)?;
        let origins = prefix(Family::VectorOrigins)?;
        if read.revision(&[&vectors, &origins])?.has_private_changes() {
            return Err(invalid(
                "journal pruning requires committed canonical input",
            ));
        }
        let scope = self.index_scope(resolver, control)?;
        let physical = crate::diskann::map_read(&read, current.database)?;
        pruner.require_selected(
            &scope,
            self.dimensions,
            self.index_parameters()
                .ok_or_else(|| invalid("missing index parameters"))?,
            &physical,
            &mut crate::diskann::map_batch(batch, current.database, &current.control)?,
            control,
        )?;
        let result = pruner.prune(
            &mut Journal {
                source: self,
                current,
                batch,
            },
            request,
            control,
        )?;
        self.check(control)?;
        Ok(result)
    }
}

struct Journal<'a> {
    source: &'a RetainedSQLiteDiskANNCanonical,
    current: &'a NativeSnapshot,
    batch: &'a mut dyn KeyValueBatch,
}

impl Journal<'_> {
    fn identity(&self) -> StorageBackendResult<Identity> {
        Identity::new(
            Family::VectorChanges,
            self.source
                .owner
                .ok_or_else(|| invalid("pruning has no canonical owner"))?,
        )
        .map_err(VersionError::into_storage_error)
    }

    fn key(
        &self,
        change: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.identity()?
            .encode_key(
                &[
                    ValueRef::Text(&self.source.field),
                    ValueRef::Blob(&change.encode()),
                ],
                control,
            )
            .map_err(VersionError::into_storage_error)
    }
}

impl DiskANNChangeJournal for Journal<'_> {
    fn next_after(
        &self,
        after: Option<DiskANNChangeIdentity>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.source.check(control)?;
        let identity = self.identity()?;
        let prefix = identity
            .encode_prefix(&[ValueRef::Text(&self.source.field)], control)
            .map_err(VersionError::into_storage_error)?;
        let cursor = after.map(|after| self.key(after, control)).transpose()?;
        let mut selected = None;
        self.current.record_read().visit_keys_after(
            &prefix,
            cursor.as_deref(),
            1,
            control,
            &mut |key| {
                self.source.check(control)?;
                if selected.is_some() {
                    return Err(invalid("native pruning cursor exceeded its page"));
                }
                let mut components = 0;
                let actual = Identity::visit_key_components(key, control, |position, value| {
                    components += 1;
                    match position {
                        0 if value == ValueRef::Text(&self.source.field) => {}
                        1 => {
                            let bytes = value.as_blob().map_err(|_| {
                                VersionError::InvalidEncoding("invalid native pruning identity")
                            })?;
                            let change = DiskANNChangeIdentity::decode(bytes)
                                .map_err(VersionError::Storage)?;
                            if change.document() > i64::MAX as DocId {
                                return Err(VersionError::InvalidEncoding(
                                    "native pruning document out of range",
                                ));
                            }
                            selected = Some(change);
                        }
                        _ => {
                            return Err(VersionError::InvalidEncoding(
                                "native pruning key escaped its field",
                            ))
                        }
                    }
                    Ok(())
                })
                .map_err(VersionError::into_storage_error)?;
                if actual != identity || components != 2 {
                    return Err(invalid("native pruning key escaped its owner"));
                }
                Ok(())
            },
        )?;
        self.source.check(control)?;
        Ok(selected)
    }

    fn change(
        &self,
        change: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNCanonicalOrigin> {
        self.source.check(control)?;
        let key = self.key(change, control)?;
        let read = self.current.record_read();
        if read
            .record_revision(&key)?
            .is_none_or(|revision| revision.has_private_changes())
        {
            return Err(invalid(
                "native pruning requires an actual committed change",
            ));
        }
        let limit = variable_fields_limit(&[
            self.source.table.len(),
            self.source.field.len(),
            CHANGE_IDENTITY_BYTES,
            CANONICAL_ORIGIN_BYTES,
        ])
        .map_err(VersionError::into_storage_error)?;
        let mut selected = None;
        read.visit_value_bounded(&key, limit, control, &mut |bytes| {
            self.source.check(control)?;
            if selected.is_some() {
                return Err(invalid("native pruning change returned repeatedly"));
            }
            let bytes = bytes.ok_or_else(|| invalid("native pruning change disappeared"))?;
            let (actual, row) =
                decode_record(&key, bytes, control).map_err(VersionError::into_storage_error)?;
            if actual != self.identity()? || row[0] != ValueRef::Text(&self.source.table) {
                return Err(invalid("native pruning row scope mismatch"));
            }
            let payload = row[3]
                .as_blob()
                .map_err(|_| invalid("invalid native pruning payload"))?;
            selected = Some(DiskANNCanonicalOrigin::decode(
                payload,
                self.source.dimensions,
            )?);
            Ok(())
        })?;
        self.source.check(control)?;
        selected.ok_or_else(|| invalid("native pruning change was not returned"))
    }

    fn current_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.source.record_on(self.current, document, control)
    }

    fn remove(
        &mut self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.source.check(control)?;
        let key = self.key(identity, control)?;
        self.batch.require_unchanged(&key)?;
        self.batch.delete(&key)
    }
}
