//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact metadata lookup admits its complete key before bounded copying.

use super::CatalogInputLimits;
use crate::{read_control::StorageReadControl, CatalogFacade, StorageBackendResult};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{JsonReadError, JsonReadWork},
    memory::{Budgeted, BudgetedString, MemoryError},
};

pub(crate) struct RetainedWork<'a, 'i> {
    pub control: &'a StorageReadControl,
    pub input: &'a mut AsyncImageInput<'i>,
}

impl JsonReadWork for RetainedWork<'_, '_> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        self.control.cancellation().check()?;
        self.input.checkpoint().await?;
        self.control.cancellation().check()?;
        self.input.cancellation.check()?;
        Ok(())
    }
}

/// Construct one exact metadata key through the caller's original work channel, then read its selected value under the same encoded limits and retained allowance.
pub async fn read_from(
    catalog: &dyn CatalogFacade,
    prefix: &str,
    name: &str,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<Budgeted<String>>> {
    let mut work = RetainedWork { control, input };
    work.checkpoint()
        .await
        .map_err(super::strings::json_error)?;
    limits.validate()?;
    let length = prefix
        .len()
        .checked_add(name.len())
        .ok_or(MemoryError::SizeOverflow)?;
    control.check_value_size(length, limits.maximum_key_bytes)?;
    let mut key = BudgetedString::new(control.memory());
    key.reserve(length)?;
    for mut text in [prefix, name] {
        while !text.is_empty() {
            work.checkpoint()
                .await
                .map_err(super::strings::json_error)?;
            let mut end = 256.min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            key.push_str(&text[..end])?;
            text = &text[end..];
        }
    }
    work.checkpoint()
        .await
        .map_err(super::strings::json_error)?;
    let value = catalog.get_metadata_with_control(&key, limits, control)?;
    control.check()?;
    work.input.cancellation.check()?;
    Ok(value)
}
