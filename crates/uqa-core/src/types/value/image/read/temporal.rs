//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Reader, ValueImageError, ValueImageResult};
use crate::TemporalValue;

impl Reader<'_> {
    pub(super) fn temporal(&mut self) -> ValueImageResult<TemporalValue> {
        Ok(match self.byte()? {
            0 => TemporalValue::Date {
                days: i32::from_le_bytes(self.fixed()?),
            },
            1 => TemporalValue::Time {
                micros: i64::from_le_bytes(self.fixed()?),
            },
            2 => TemporalValue::TimeTz {
                micros: i64::from_le_bytes(self.fixed()?),
                offset_seconds: i32::from_le_bytes(self.fixed()?),
            },
            3 => TemporalValue::Timestamp {
                postgres_micros: i64::from_le_bytes(self.fixed()?),
            },
            4 => TemporalValue::TimestampTz {
                postgres_micros: i64::from_le_bytes(self.fixed()?),
            },
            5 => TemporalValue::Interval {
                months: i32::from_le_bytes(self.fixed()?),
                days: i32::from_le_bytes(self.fixed()?),
                micros: i64::from_le_bytes(self.fixed()?),
            },
            _ => return Err(ValueImageError::Invalid("temporal tag")),
        })
    }
}
