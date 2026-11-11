//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical provenance bytes retain container order and bounded string serialization.

use super::{DictionaryResult, Value, Work};

pub(crate) async fn canonical_with(
    value: &Value,
    work: &mut impl Work,
) -> DictionaryResult<Vec<u8>> {
    let mut output = Vec::new();
    crate::json::write_value(value, &mut output, work).await?;
    Ok(output)
}
