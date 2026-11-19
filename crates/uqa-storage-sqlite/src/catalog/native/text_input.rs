//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog text uses the same row-key validation as ordinary reads.

use super::{string_ref, text, Catalog, Family, NativeRecordOwner};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use rusqlite::types::ValueRef;
use uqa_core::memory::{Budgeted, BudgetedString};
use uqa_storage::{
    catalog::text_input::{
        CatalogTextDecoder, CatalogTextFamily, CatalogTextInput, CatalogTextLimits, CatalogTextRow,
    },
    key_value::KeyValueRead,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_metadata_input(
        &self,
        name_prefix: &str,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTextInput> {
        control.check()?;
        limits.validate()?;
        control.check_value_size(name_prefix.len(), limits.maximum_key_bytes)?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained catalog input requires native records".into())
        })?;
        let prefix = NativeRecordIdentity::new(
            Family::Metadata,
            NativeRecordOwner::Database(snapshot.database),
        )
        .and_then(|identity| identity.encode_text_prefix(name_prefix.as_bytes(), control))
        .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&prefix])?;
        CatalogTextInput::new(read, &prefix, decode, limits, control)
    }

    pub(in crate::catalog) fn native_metadata_with_control(
        &self,
        name: &str,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Budgeted<String>>> {
        control.check()?;
        limits.validate()?;
        control.check_value_size(name.len(), limits.maximum_key_bytes)?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("controlled metadata reads require native records".into())
        })?;
        let identity = NativeRecordIdentity::new(
            Family::Metadata,
            NativeRecordOwner::Database(snapshot.database),
        )
        .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        let key = identity
            .encode_key(&[text(name)], control)
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        control.check_value_size(key.len(), limits.maximum_key_bytes)?;
        let read = snapshot.record_read();
        let mut result = None;
        read.visit_value_bounded(&key, limits.maximum_value_bytes, control, &mut |value| {
            if let Some(value) = value {
                let (_, row) = decode_record(&key, value, control)
                    .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
                let mut owned = BudgetedString::new(control.memory());
                owned.push_str(string_ref(row[1])?)?;
                let (text, memory) = owned.into_parts();
                result = Some(Budgeted::new(text, memory));
            }
            Ok(())
        })?;
        control.check()?;
        read.control().check()?;
        Ok(result)
    }

    pub(in crate::catalog) fn native_text_input(
        &self,
        family: CatalogTextFamily,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTextInput> {
        let (family, decoder): (_, CatalogTextDecoder) = match family {
            CatalogTextFamily::Metadata => (Family::Metadata, decode),
            CatalogTextFamily::Models => (Family::Models, decode),
            CatalogTextFamily::ScoringParameters => (Family::ScoringParams, decode),
            CatalogTextFamily::Analyzers => (Family::Analyzers, decode),
            CatalogTextFamily::AnalyzerDescriptors => (Family::Analyzers, decode_descriptor),
            CatalogTextFamily::PathIndexes => (Family::PathIndexes, decode),
        };
        self.native_row_input(family, decoder, limits, control)
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
