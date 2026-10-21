//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal type selection is immutable; conversion still reads live session settings.

use super::super::temporal::{self, TemporalCastTarget};
use super::{ColumnType, Produced, ProductionControl, Result, Value};
use crate::ast::IntervalFields;

#[derive(Clone, Copy, Debug)]
pub(super) struct TemporalCast {
    target: TemporalCastTarget,
    precision: Option<u32>,
    fields: Option<IntervalFields>,
}

impl TemporalCast {
    pub(super) fn select(target: &ColumnType) -> Option<Self> {
        use ColumnType as T;
        let (target, precision, fields) = match target {
            T::Date => (TemporalCastTarget::Date, None, None),
            T::Time => (TemporalCastTarget::Time, None, None),
            T::TimePrecision(p) => (TemporalCastTarget::Time, Some(*p), None),
            T::TimeTz => (TemporalCastTarget::TimeTz, None, None),
            T::TimeTzPrecision(p) => (TemporalCastTarget::TimeTz, Some(*p), None),
            T::Timestamp => (TemporalCastTarget::Timestamp, None, None),
            T::TimestampPrecision(p) => (TemporalCastTarget::Timestamp, Some(*p), None),
            T::TimestampTz => (TemporalCastTarget::TimestampTz, None, None),
            T::TimestampTzPrecision(p) => (TemporalCastTarget::TimestampTz, Some(*p), None),
            T::Interval => (TemporalCastTarget::Interval, None, None),
            T::IntervalWithFields { fields, precision } => {
                (TemporalCastTarget::Interval, *precision, Some(*fields))
            }
            _ => return None,
        };
        Some(Self {
            target,
            precision,
            fields,
        })
    }

    pub(super) fn column_type(self) -> ColumnType {
        use ColumnType as T;
        match (self.target, self.precision) {
            (TemporalCastTarget::Date, _) => T::Date,
            (TemporalCastTarget::Time, None) => T::Time,
            (TemporalCastTarget::Time, Some(p)) => T::TimePrecision(p),
            (TemporalCastTarget::TimeTz, None) => T::TimeTz,
            (TemporalCastTarget::TimeTz, Some(p)) => T::TimeTzPrecision(p),
            (TemporalCastTarget::Timestamp, None) => T::Timestamp,
            (TemporalCastTarget::Timestamp, Some(p)) => T::TimestampPrecision(p),
            (TemporalCastTarget::TimestampTz, None) => T::TimestampTz,
            (TemporalCastTarget::TimestampTz, Some(p)) => T::TimestampTzPrecision(p),
            (TemporalCastTarget::Interval, precision) => {
                self.fields
                    .map_or(T::Interval, |fields| T::IntervalWithFields {
                        fields,
                        precision,
                    })
            }
        }
    }

    pub(super) fn evaluate(
        self,
        value: &Value,
        source: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        let mut value = temporal::cast_temporal(value, source, self.target, None, control)?;
        if let Some(fields) = self.fields {
            value = temporal::restrict_interval(value, fields, self.precision)?;
        } else if let Some(precision) = self.precision {
            temporal::round_temporal_precision(&mut value, precision)?;
        }
        Ok(control.finish(value, control.empty_reservation())?)
    }
}
