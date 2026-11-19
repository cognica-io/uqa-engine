//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy reservation text is consumed in fixed fragments without narrowing its sentinel.

use super::LEGACY_KEY_PREFIX;
use crate::{
    catalog::input::{metadata, strings, CatalogInputLimits},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};
use uqa_core::{image_io::AsyncImageInput, json::JsonReadWork};

/// Read the selected reservation with the same absent/empty and u128 semantics as the ordinary loader. Leading zeros and the optional plus sign remain valid, regardless of text length. Visible document maxima are combined by `restored_document_id_watermark` separately.
pub async fn load_legacy_document_id_watermark_from(
    catalog: &dyn CatalogFacade,
    table: &str,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<u128>> {
    let Some(value) =
        metadata::read_from(catalog, LEGACY_KEY_PREFIX, table, limits, control, input).await?
    else {
        return Ok(None);
    };
    parse(&value, table, control, input).await
}

async fn parse(
    value: &str,
    table: &str,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<u128>> {
    let mut work = metadata::RetainedWork { control, input };
    work.checkpoint().await.map_err(strings::json_error)?;
    if value.is_empty() {
        return Ok(None);
    }
    let digits = value.strip_prefix('+').unwrap_or(value).as_bytes();
    if digits.is_empty() {
        return Err(invalid(table, false));
    }
    let mut number = 0u128;
    for fragment in digits.chunks(256) {
        work.checkpoint().await.map_err(strings::json_error)?;
        for byte in fragment {
            let digit = byte.wrapping_sub(b'0');
            if digit > 9 {
                return Err(invalid(table, false));
            }
            number = number
                .checked_mul(10)
                .and_then(|number| number.checked_add(u128::from(digit)))
                .ok_or_else(|| invalid(table, true))?;
        }
    }
    Ok(Some(number))
}

fn invalid(table: &str, overflow: bool) -> StorageBackendError {
    let error = if overflow {
        "340282366920938463463374607431768211456"
    } else {
        "x"
    }
    .parse::<u128>()
    .unwrap_err();
    StorageBackendError::Other(format!(
        "invalid persisted next id for table `{table}`: {error}"
    ))
}

#[cfg(test)]
mod tests;
