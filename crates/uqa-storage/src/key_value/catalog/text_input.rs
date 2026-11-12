//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named text rows borrow the existing native key and UTF-8 codecs.

use super::{
    KeyValueCatalog, TAG_ANALYZER, TAG_ANALYZER_DESCRIPTOR, TAG_METADATA, TAG_MODEL,
    TAG_PATH_INDEX, TAG_SCORING_PARAMS,
};
use crate::{
    catalog::text_input::{CatalogTextFamily, CatalogTextInput, CatalogTextLimits, CatalogTextRow},
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedString, MemoryError};

impl KeyValueCatalog {
    pub(super) fn metadata_with_control(
        &self,
        name: &str,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Budgeted<String>>> {
        control.check()?;
        limits.validate()?;
        control.check_value_size(
            name.len().checked_add(5).ok_or(MemoryError::SizeOverflow)?,
            limits.maximum_key_bytes,
        )?;
        let key = crate::key_value::codec::single_str_key_prefix(
            TAG_METADATA,
            name,
            name.len(),
            control.memory(),
        )?;
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            let mut result = None;
            read.visit_value_bounded(&key, limits.maximum_value_bytes, control, &mut |value| {
                if let Some(value) = value {
                    let text = std::str::from_utf8(value).map_err(|error| {
                        crate::key_value::codec::other_error(format!(
                            "invalid utf-8 value: {error}"
                        ))
                    })?;
                    let mut owned = BudgetedString::new(control.memory());
                    owned.push_str(text)?;
                    let (text, memory) = owned.into_parts();
                    result = Some(Budgeted::new(text, memory));
                }
                Ok(())
            })?;
            control.check()?;
            read.control().check()?;
            Ok(result)
        })
    }

    pub(super) fn text_input(
        &self,
        family: CatalogTextFamily,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTextInput> {
        let tag = match family {
            CatalogTextFamily::Metadata => TAG_METADATA,
            CatalogTextFamily::Models => TAG_MODEL,
            CatalogTextFamily::ScoringParameters => TAG_SCORING_PARAMS,
            CatalogTextFamily::Analyzers => TAG_ANALYZER,
            CatalogTextFamily::AnalyzerDescriptors => TAG_ANALYZER_DESCRIPTOR,
            CatalogTextFamily::PathIndexes => TAG_PATH_INDEX,
        };
        self.row_input(
            &[tag],
            if matches!(family, CatalogTextFamily::Metadata) {
                decode_metadata
            } else {
                decode
            },
            limits,
            control,
        )
    }
}

fn decode_metadata(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    CatalogTextRow::copy(
        super::schema_table::metadata_key(key)?,
        super::schema_table::metadata_value(value)?,
        control,
    )
    .map(Some)
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    let mut offset = 1;
    let name = crate::key_value::codec::read_str_ref(key, &mut offset)?;
    let value = std::str::from_utf8(value).map_err(|error| {
        crate::key_value::codec::other_error(format!("invalid utf-8 value: {error}"))
    })?;
    CatalogTextRow::copy(name, value, control).map(Some)
}
