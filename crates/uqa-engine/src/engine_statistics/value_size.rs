//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound sampled and persisted statistics without truncating actual values.

use std::io::{self, Write};

use uqa_core::Value;

/// Like `PostgreSQL`'s scalar analysis, very wide values are omitted from
/// ordered and most-common-value statistics, not copied into the catalog.
pub(crate) const TEXT_BYTES: usize = 1_024;
pub(crate) const ENCODED_VALUE_BYTES: usize = 8_192;
pub(crate) const HISTOGRAM_VALUES: usize = 101;
pub(crate) const MCV_VALUES: usize = 10;
pub(super) const FORMAT_VERSION: u32 = 1;

struct LimitedWriter(usize);

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("statistics value exceeds its encoded size budget"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn accepts(value: &Value) -> bool {
    match value {
        Value::Str(text) | Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text) => {
            text.len() <= TEXT_BYTES
        }
        Value::Bytes(bytes) => bytes.len() <= TEXT_BYTES,
        Value::Null | Value::Void | Value::Bool(_) | Value::Int(_) | Value::Float(_) => true,
        // Count encoded bytes without allocating another serialized copy.
        _ => serde_json::to_writer(&mut LimitedWriter(ENCODED_VALUE_BYTES), value).is_ok(),
    }
}

pub(crate) const fn encoded_list_bytes(values: usize) -> usize {
    2 + values * (ENCODED_VALUE_BYTES + 1)
}
