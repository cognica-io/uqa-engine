//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Value;
use crate::memory::{AllocationObserver, ObserveAllocations};
use crate::ValueRetentionError;

impl ObserveAllocations for Value {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Str(value) | Self::FixedChar(value) | Self::Json(value) | Self::JsonB(value) => {
                observer.child(value)
            }
            Self::Bytes(value) => observer.buffer::<u8>(value.len(), value.capacity()),
            Self::Array(value) => observer.child(value),
            Self::LegacyVector(value) => observer.child(value.as_array()),
            Self::List(value) => observer.child(value),
            Self::Row(value) => observer.child(value),
            Self::Record(value) => observer.child(value),
            Self::BitString(value) => observer.child(value),
            Self::Enum(value) => observer.child(value),
            Self::Datum(value) => observer.child(value),
            Self::Map(value) => observer.value_map(value),
            // BigInt's private capacity is not observable; its conservative admission bound is not a measurement.
            Self::Decimal(_)
            | Self::Null
            | Self::Void
            | Self::Bool(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::Temporal(_) => Ok(()),
        }
    }
}
