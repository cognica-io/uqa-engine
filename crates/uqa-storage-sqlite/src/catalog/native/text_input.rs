//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog text uses the same row-key validation as ordinary reads.

use super::{string_ref, Catalog, Family, NativeRecordOwner};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use rusqlite::types::ValueRef;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    catalog::text_input::{
        CatalogTextDecoder, CatalogTextFamily, CatalogTextInput, CatalogTextLimits, CatalogTextRow,
    },
    key_value::KeyValueRead,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_text_input(
        &self,
        family: CatalogTextFamily,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTextInput> {
        control.check()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained catalog text input requires native records".into())
        })?;
        let (family, decoder): (_, CatalogTextDecoder) = match family {
            CatalogTextFamily::Metadata => (Family::Metadata, decode),
            CatalogTextFamily::Models => (Family::Models, decode),
            CatalogTextFamily::ScoringParameters => (Family::ScoringParams, decode),
            CatalogTextFamily::Analyzers => (Family::Analyzers, decode),
            CatalogTextFamily::AnalyzerDescriptors => (Family::Analyzers, decode_descriptor),
            CatalogTextFamily::PathIndexes => (Family::PathIndexes, decode),
        };
        let prefix =
            NativeRecordIdentity::new(family, NativeRecordOwner::Database(snapshot.database))
                .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?
                .encode_prefix(&[], control)
                .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&prefix])?;
        CatalogTextInput::new(read, &prefix, decoder, limits, control)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    decode_column(key, value, control, 1, false)
}

fn decode_descriptor(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    decode_column(key, value, control, 2, true)
}

fn decode_column(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
    column: usize,
    nonnull: bool,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    let (_, row) = decode_record(key, value, control)
        .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
    if nonnull && row[column] == ValueRef::Null {
        return Ok(None);
    }
    CatalogTextRow::copy(string_ref(row[0])?, string_ref(row[column])?, control).map(Some)
}
