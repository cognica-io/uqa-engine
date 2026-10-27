//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal comparison, equality and predecessor reservation encodings share one value owner.

use super::{TemporalInfinity, TemporalValue, MICROS_PER_DAY, POSTGRES_EPOCH_MICROS};

impl TemporalValue {
    /// Append a lexicographically ordered key. `TIME` retains its day endpoint; `TIMETZ` orders adjusted time, then its original zone. The caller controls output allocation and errors.
    pub fn write_comparison_key<E>(
        &self,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let (kind, rank, zone) = self.sort_key();
        write(&[kind])?;
        write(&((rank as u128) ^ (1_u128 << 127)).to_be_bytes())?;
        if matches!(self, Self::TimeTz { .. }) {
            write(&((zone as u64) ^ (1_u64 << 63)).to_be_bytes())?;
        }
        Ok(())
    }

    /// Append an equality key consistent with native comparison and the current timestamp epoch and zone precision. The caller owns the sink and its allocation.
    pub fn write_equality_key<E>(
        &self,
        write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.write_native_equality_key(write)
    }

    /// Emit the predecessor's day-wrapped key only for conflict-reservation aliases during an upgrade. These bytes must never determine current equality or ordering.
    pub fn write_legacy_reservation_key<E>(
        &self,
        write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<bool, E> {
        self.write_predecessor_reservation_key(true, write)
    }

    /// Emit the predecessor's Unix timestamp and whole-minute zone lock address. A value outside that carrier has no predecessor alias and writes nothing.
    pub fn write_unix_reservation_key<E>(
        &self,
        write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<bool, E> {
        self.write_predecessor_reservation_key(false, write)
    }

    fn write_predecessor_reservation_key<E>(
        &self,
        day_wrapped: bool,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<bool, E> {
        let (kind, rank, zone) = self.sort_key();
        match self {
            Self::Timestamp { postgres_micros } | Self::TimestampTz { postgres_micros } => {
                let micros = match self.infinity() {
                    Some(TemporalInfinity::Negative) => i64::MIN,
                    Some(TemporalInfinity::Positive) => i64::MIN + 1,
                    None => match postgres_micros.checked_add(POSTGRES_EPOCH_MICROS) {
                        Some(value) if value > i64::MIN + 1 => value,
                        _ => return Ok(false),
                    },
                };
                write(&[kind])?;
                write(&micros.to_be_bytes())?;
            }
            Self::TimeTz { offset_seconds, .. } => {
                if offset_seconds % 60 != 0 {
                    return Ok(false);
                }
                write(&[kind])?;
                if day_wrapped {
                    write(&rank.rem_euclid(i128::from(MICROS_PER_DAY)).to_be_bytes())?;
                } else {
                    write(&rank.to_be_bytes())?;
                    write(&(zone / 60).to_be_bytes())?;
                }
            }
            Self::Time { .. } if day_wrapped => {
                write(&[kind])?;
                write(&rank.rem_euclid(i128::from(MICROS_PER_DAY)).to_be_bytes())?;
            }
            _ => self.write_native_equality_key(write)?,
        }
        Ok(true)
    }

    fn write_native_equality_key<E>(
        &self,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let (kind, rank, zone) = self.sort_key();
        write(&[kind])?;
        match self {
            Self::Date { days } => write(&days.to_be_bytes())?,
            Self::Timestamp {
                postgres_micros: micros,
            }
            | Self::TimestampTz {
                postgres_micros: micros,
            } => {
                write(&micros.to_be_bytes())?;
            }
            Self::TimeTz { .. } => {
                write(&rank.to_be_bytes())?;
                write(&zone.to_be_bytes())?;
            }
            Self::Time { .. } | Self::Interval { .. } => write(&rank.to_be_bytes())?,
        }
        Ok(())
    }
}
