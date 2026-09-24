//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal comparison, equality and predecessor reservation encodings share one value owner.

use super::{TemporalValue, MICROS_PER_DAY};

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

    /// Append an equality key consistent with native comparison, preserving the established bytes for non-time families. The caller owns the sink and its allocation.
    pub fn write_equality_key<E>(
        &self,
        write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.write_equality_key_version(false, write)
    }

    /// Emit the predecessor's day-wrapped key only for conflict-reservation aliases during an upgrade. These bytes must never determine current equality or ordering.
    pub fn write_legacy_reservation_key<E>(
        &self,
        write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.write_equality_key_version(true, write)
    }

    fn write_equality_key_version<E>(
        &self,
        legacy: bool,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let (kind, rank, zone) = self.sort_key();
        write(&[kind])?;
        match self {
            Self::Date { days } => write(&days.to_be_bytes())?,
            Self::Timestamp { micros } | Self::TimestampTz { micros } => {
                write(&micros.to_be_bytes())?;
            }
            Self::Time { .. } | Self::TimeTz { .. } if legacy => {
                write(&rank.rem_euclid(i128::from(MICROS_PER_DAY)).to_be_bytes())?;
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
