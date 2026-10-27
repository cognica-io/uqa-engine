//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-type date operators compare civil midnight without narrowing its range.

use std::cmp::Ordering;

use crate::error::Result;
use crate::expr::EngineHook;
use uqa_core::{memory::ProductionControl, TemporalInfinity, TemporalTimeZone, TemporalValue};

pub(super) fn compare(
    left: &TemporalValue,
    right: &TemporalValue,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Ordering>> {
    let (date, timestamp, reversed, zoned) = match (left, right) {
        (TemporalValue::Date { .. }, TemporalValue::Timestamp { .. }) => {
            (left, right, false, false)
        }
        (TemporalValue::Date { .. }, TemporalValue::TimestampTz { .. }) => {
            (left, right, false, true)
        }
        (TemporalValue::Timestamp { .. }, TemporalValue::Date { .. }) => (right, left, true, false),
        (TemporalValue::TimestampTz { .. }, TemporalValue::Date { .. }) => {
            (right, left, true, true)
        }
        _ => return Ok(None),
    };
    control.check()?;
    let order = match (date.infinity(), timestamp.infinity()) {
        (Some(left), Some(right)) => infinity_rank(left).cmp(&infinity_rank(right)),
        (Some(TemporalInfinity::Negative), _) | (_, Some(TemporalInfinity::Positive)) => {
            Ordering::Less
        }
        (Some(TemporalInfinity::Positive), _) | (_, Some(TemporalInfinity::Negative)) => {
            Ordering::Greater
        }
        (None, None) => {
            let TemporalValue::Date { days } = date else {
                unreachable!("date comparison operand")
            };
            let (TemporalValue::Timestamp {
                postgres_micros: micros,
            }
            | TemporalValue::TimestampTz {
                postgres_micros: micros,
            }) = timestamp
            else {
                unreachable!("timestamp comparison operand")
            };
            let seconds = i64::from(*days) * 86_400;
            let offset = if zoned {
                let zone = engine
                    .map(EngineHook::temporal_time_zone)
                    .transpose()?
                    .flatten()
                    .unwrap_or_else(|| TemporalTimeZone::fixed(0).expect("UTC offset"));
                control.check()?;
                zone.offset_for_local(seconds)
                    .ok_or_else(|| crate::expr::datetime_out_of_range("timestamp"))?
            } else {
                0
            };
            let instant = (i128::from(seconds) - i128::from(offset)) * 1_000_000;
            (instant - i128::from(uqa_core::POSTGRES_EPOCH_MICROS)).cmp(&i128::from(*micros))
        }
    };
    Ok(Some(if reversed { order.reverse() } else { order }))
}

fn infinity_rank(value: TemporalInfinity) -> i8 {
    match value {
        TemporalInfinity::Negative => -1,
        TemporalInfinity::Positive => 1,
    }
}

#[cfg(test)]
mod tests;
