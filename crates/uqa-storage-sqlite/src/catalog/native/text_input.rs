//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog text uses the same row-key validation as ordinary reads.

use super::{string_ref, Catalog, Family};
use crate::mvcc::native::decode_record;
use rusqlite::types::ValueRef;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    catalog::text_input::{
        CatalogTextDecoder, CatalogTextFamily, CatalogTextInput, CatalogTextLimits, CatalogTextRow,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};

impl Catalog {
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
