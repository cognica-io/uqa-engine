//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native envelopes preserve the common binary key order and borrowed values.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{DatabaseId, VersionError};
use uqa_storage::read_control::{KeyReadVisitor, KeyValueReadVisitor, StorageReadControl};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::mvcc::native::{
    decode_record, NativeRecord, NativeRecordFamily, NativeRecordIdentity, NativeRecordOwner,
};

#[derive(Clone, Copy)]
pub(super) struct Mapping(NativeRecordIdentity);

impl Mapping {
    pub(super) fn new(namespace: DatabaseId) -> StorageBackendResult<Self> {
        NativeRecordIdentity::new(
            NativeRecordFamily::DiskANNRecords,
            NativeRecordOwner::Database(namespace),
        )
        .map(Self)
        .map_err(VersionError::into_storage_error)
    }

    pub(super) fn key(
        self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.0
            .encode_key(&[ValueRef::Blob(key)], control)
            .map_err(VersionError::into_storage_error)
    }

    pub(super) fn prefix(
        self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.0
            .encode_blob_prefix(prefix, control)
            .map_err(VersionError::into_storage_error)
    }

    pub(super) fn record(
        self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<NativeRecord> {
        NativeRecord::encode(
            self.0.family(),
            self.0.owner(),
            &[ValueRef::Blob(key), ValueRef::Blob(value)],
            control,
        )
        .map_err(VersionError::into_storage_error)
    }

    pub(super) fn visit_value(
        self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let (identity, row) =
            decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
        if identity != self.0 {
            return Err(invalid());
        }
        let [ValueRef::Blob(logical), ValueRef::Blob(payload)] = &*row else {
            return Err(invalid());
        };
        visit(logical, payload)
    }

    pub(super) fn visit_key(
        self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if NativeRecordIdentity::decode(key).map_err(VersionError::into_storage_error)? != self.0 {
            return Err(invalid());
        }
        // Decode all components before publishing the result to the callback.
        let mut logical = BudgetedVec::new(control.memory());
        NativeRecordIdentity::visit_key_components(key, control, |_, value| {
            let ValueRef::Blob(bytes) = value else {
                return Err(VersionError::InvalidEncoding(
                    "native DiskANN key is not binary",
                ));
            };
            logical.extend_from_slice(bytes)?;
            Ok(())
        })
        .map_err(VersionError::into_storage_error)?;
        visit(&logical)
    }
}

pub(super) fn invalid() -> StorageBackendError {
    VersionError::InvalidEncoding("native DiskANN record identity or payload mismatch")
        .into_storage_error()
}
