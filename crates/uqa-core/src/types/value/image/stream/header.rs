//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::ValueImageResult;
use crate::{memory::MemoryError, TemporalValue};

pub(super) struct Header {
    bytes: [u8; 32],
    length: usize,
}

impl Header {
    pub(super) fn new() -> Self {
        Self {
            bytes: [0; 32],
            length: 0,
        }
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) {
        self.bytes[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
    }

    pub(super) fn byte(&mut self, byte: u8) {
        self.bytes(&[byte]);
    }

    pub(super) fn length(&mut self, length: usize) -> ValueImageResult<()> {
        self.bytes(
            &u64::try_from(length)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        );
        Ok(())
    }

    pub(super) fn optional_oid(&mut self, oid: Option<u32>) {
        self.byte(u8::from(oid.is_some()));
        if let Some(oid) = oid {
            self.bytes(&oid.to_le_bytes());
        }
    }

    pub(super) fn temporal(&mut self, value: &TemporalValue) {
        match value {
            TemporalValue::Date { days } => {
                self.byte(0);
                self.bytes(&days.to_le_bytes());
            }
            TemporalValue::Time { micros } => {
                self.byte(1);
                self.bytes(&micros.to_le_bytes());
            }
            TemporalValue::TimeTz {
                micros,
                offset_seconds,
            } => {
                self.byte(2);
                self.bytes(&micros.to_le_bytes());
                self.bytes(&offset_seconds.to_le_bytes());
            }
            TemporalValue::Timestamp { postgres_micros } => {
                self.byte(3);
                self.bytes(&postgres_micros.to_le_bytes());
            }
            TemporalValue::TimestampTz { postgres_micros } => {
                self.byte(4);
                self.bytes(&postgres_micros.to_le_bytes());
            }
            TemporalValue::Interval {
                months,
                days,
                micros,
            } => {
                self.byte(5);
                self.bytes(&months.to_le_bytes());
                self.bytes(&days.to_le_bytes());
                self.bytes(&micros.to_le_bytes());
            }
        }
    }
}
