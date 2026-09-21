//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared temporal families reuse native parsing and comparison for logical index keys.

use uqa_core::{memory::MemoryError, TemporalValue};
use uqa_sql::{ast::ColumnType, SQLError};
use uqa_storage::read_control::StorageReadControl;

use super::{check, resource_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalIndexDomain {
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
}

impl TemporalIndexDomain {
    pub(super) fn from_column_type(ty: &ColumnType) -> Option<Self> {
        Some(match ty {
            ColumnType::Date => Self::Date,
            ColumnType::Time | ColumnType::TimePrecision(_) => Self::Time,
            ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => Self::TimeTz,
            ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => Self::Timestamp,
            ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => Self::TimestampTz,
            ColumnType::Interval | ColumnType::IntervalWithFields { .. } => Self::Interval,
            _ => return None,
        })
    }

    pub(super) fn sample(self) -> TemporalValue {
        match self {
            Self::Date => TemporalValue::Date { days: 0 },
            Self::Time => TemporalValue::Time { micros: 0 },
            Self::TimeTz => TemporalValue::TimeTz {
                micros: 0,
                offset_minutes: 0,
            },
            Self::Timestamp => TemporalValue::Timestamp { micros: 0 },
            Self::TimestampTz => TemporalValue::TimestampTz { micros: 0 },
            Self::Interval => TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: 0,
            },
        }
    }

    pub(super) fn parse(
        self,
        text: &str,
        control: &StorageReadControl,
    ) -> Result<Option<TemporalValue>, SQLError> {
        check(control)?;
        // Cover native parsing's input-sized string copies and both token-slice buffers before allocation. The reservation is shared with the original reader and released before returning the inline temporal value.
        let bytes = text
            .len()
            .checked_add(1)
            .and_then(|len| len.checked_mul(4 + 2 * std::mem::size_of::<&str>()))
            .and_then(|bytes| bytes.checked_add(256))
            .ok_or_else(|| resource_error(MemoryError::SizeOverflow))?;
        let _workspace = control.memory().reserve(bytes).map_err(resource_error)?;
        let value = self.sample().parse_same_kind(text);
        check(control)?;
        Ok(value)
    }
}
