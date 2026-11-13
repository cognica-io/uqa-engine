//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original field keys and UTF-8 values share one retained Key/Value view.

use super::{KeyValueCatalog, TAG_FIELD_ANALYZER_BINDING, TAG_TABLE_FIELD_ANALYZER};
use crate::{
    catalog::{
        analyzer_input::{
            CatalogAnalyzerBindingRow, CatalogAnalyzerInput, CatalogAnalyzerLabelRow,
        },
        input::CatalogInputLimits,
    },
    key_value::codec::{other_error, read_str_ref},
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::memory::Budgeted;

impl KeyValueCatalog {
    pub(super) fn analyzer_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogAnalyzerInput> {
        control.check()?;
        limits.validate()?;
        if !self.store.transaction_model().is_versioned() {
            return Err(other_error(
                "retained analyzer input requires a versioned read owner",
            ));
        }
        let read = crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            read.retain(&[&[TAG_FIELD_ANALYZER_BINDING], &[TAG_TABLE_FIELD_ANALYZER]])
        })?;
        CatalogAnalyzerInput::new(
            read,
            (&[TAG_FIELD_ANALYZER_BINDING], binding),
            (&[TAG_TABLE_FIELD_ANALYZER], label),
            limits,
            control,
        )
    }
}

fn binding(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerBindingRow>>> {
    control.check()?;
    let mut offset = 1;
    let table = read_str_ref(key, &mut offset)?;
    let field = read_str_ref(key, &mut offset)?;
    if offset != key.len() {
        return Err(other_error("invalid analyzer binding key"));
    }
    CatalogAnalyzerBindingRow::copy(table, field, text(value)?, control).map(Some)
}

fn label(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogAnalyzerLabelRow>>> {
    control.check()?;
    let mut offset = 1;
    let table = read_str_ref(key, &mut offset)?;
    let field = read_str_ref(key, &mut offset)?;
    let phase = read_str_ref(key, &mut offset)?;
    CatalogAnalyzerLabelRow::copy(table, field, phase, text(value)?, control).map(Some)
}

fn text(bytes: &[u8]) -> StorageBackendResult<&str> {
    std::str::from_utf8(bytes).map_err(|error| other_error(format!("invalid utf-8 value: {error}")))
}

#[cfg(test)]
mod tests;
