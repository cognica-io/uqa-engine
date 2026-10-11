//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Non-finite temporal arithmetic preserves endpoint signs and rejects indeterminate results.

use super::datetime_out_of_range;
use crate::error::Result;
use uqa_core::{TemporalInfinity, TemporalValue};

pub(super) fn add(
    left: Option<TemporalInfinity>,
    right: Option<TemporalInfinity>,
    type_name: &str,
) -> Result<Option<TemporalInfinity>> {
    match (left, right) {
        (Some(left), Some(right)) if left != right => Err(datetime_out_of_range(type_name)),
        _ => Ok(left.or(right)),
    }
}

pub(super) fn subtract(
    left: Option<TemporalInfinity>,
    right: Option<TemporalInfinity>,
) -> Result<Option<TemporalInfinity>> {
    add(left, right.map(TemporalInfinity::negate), "interval")
}

pub(super) fn timestamp_micros(infinity: TemporalInfinity) -> i64 {
    let TemporalValue::Timestamp { micros } = infinity.timestamp() else {
        unreachable!("timestamp endpoint")
    };
    micros
}
