//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound sampled and persisted statistics without truncating actual values.

use std::io::{self, Write};

use uqa_core::{memory::ProductionControl, Value, ValueRetentionError, ValueSerialization};

/// Like `PostgreSQL`'s scalar analysis, very wide values are omitted from
/// ordered and most-common-value statistics, not copied into the catalog.
pub const TEXT_BYTES: usize = 1_024;
pub const ENCODED_VALUE_BYTES: usize = 8_192;
pub const HISTOGRAM_VALUES: usize = 101;
pub const MCV_VALUES: usize = 10;
pub const FORMAT_VERSION: u32 = 1;

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

pub fn accepts(value: &Value) -> bool {
    direct(value).unwrap_or_else(|| {
        // Count encoded bytes without allocating another serialized copy.
        serde_json::to_writer(&mut LimitedWriter(ENCODED_VALUE_BYTES), value).is_ok()
    })
}

/// Apply the same width predicate while retaining any numeric or nested byte-formatting scratch. Resource failures remain errors instead of silently omitting a value.
pub fn accepts_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<bool, ValueRetentionError> {
    control.check()?;
    if let Some(accepted) = direct(value) {
        return Ok(accepted);
    }
    if control.budget().is_none() {
        return Ok(accepts(value));
    }
    let context = ValueSerialization::new(*control);
    let accepted = serde_json::to_writer(
        &mut LimitedWriter(ENCODED_VALUE_BYTES),
        &context.value(value),
    )
    .is_ok();
    if let Some(error) = context.take_error() {
        return Err(error);
    }
    control.check()?;
    Ok(accepted)
}

fn direct(value: &Value) -> Option<bool> {
    match value {
        Value::Str(text) | Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text) => {
            Some(text.len() <= TEXT_BYTES)
        }
        Value::Bytes(bytes) => Some(bytes.len() <= TEXT_BYTES),
        Value::Null | Value::Void | Value::Bool(_) | Value::Int(_) | Value::Float(_) => Some(true),
        _ => None,
    }
}

pub const fn encoded_list_bytes(values: usize) -> usize {
    2 + values * (ENCODED_VALUE_BYTES + 1)
}
