//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Reader, ValueImageError, ValueImageResult};
use crate::TemporalValue;

impl Reader<'_, '_> {
    pub(super) async fn temporal(&mut self) -> ValueImageResult<TemporalValue> {
        Ok(match self.byte().await? {
            0 => TemporalValue::Date {
                days: i32::from_le_bytes(self.fixed().await?),
            },
            1 => TemporalValue::Time {
                micros: i64::from_le_bytes(self.fixed().await?),
            },
            2 => TemporalValue::TimeTz {
                micros: i64::from_le_bytes(self.fixed().await?),
                offset_seconds: i32::from_le_bytes(self.fixed().await?),
            },
            3 => TemporalValue::Timestamp {
                postgres_micros: i64::from_le_bytes(self.fixed().await?),
            },
            4 => TemporalValue::TimestampTz {
                postgres_micros: i64::from_le_bytes(self.fixed().await?),
            },
            5 => TemporalValue::Interval {
                months: i32::from_le_bytes(self.fixed().await?),
                days: i32::from_le_bytes(self.fixed().await?),
                micros: i64::from_le_bytes(self.fixed().await?),
            },
            _ => return Err(ValueImageError::Invalid("temporal tag")),
        })
    }
}
