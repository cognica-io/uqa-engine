//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Monotonic identifier reservations are independent of record visibility and transaction undo.

mod conformance;
pub use conformance::{verify_identifier_allocations, verify_identifier_batches};

use std::{num::NonZeroU64, ops::RangeInclusive};

use uqa_core::memory::{MemoryError, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendResult};

use super::{VersionError, VersionResult};

/// Session-bound access to durable, nontransactional identifier reservations. Implementations retain their session's cancellation, memory and read-only checks; forwarding storage wrappers must preserve this capability.
pub trait IdentifierAllocator: Send + Sync {
    /// Read the current autonomous watermark without reserving an identity or creating a namespace. This read is independent of the logical record snapshot and remains available to read-only sessions.
    fn identifier_watermark(&self, namespace: &[u8]) -> StorageBackendResult<Option<u64>>;

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: IdentifierRequest,
    ) -> StorageBackendResult<IdentifierAllocation>;
}

/// Observe an externally supplied identity or reserve a contiguous range. Namespace keys must include the allocation domain and the owning object's non-reused generation, rather than its reusable name.
#[derive(Debug, Clone, Copy)]
pub enum IdentifierRequest {
    Observe(u64),
    Reserve {
        minimum: u64,
        maximum: u64,
        count: NonZeroU64,
    },
}

/// The new durable high watermark and, for a reservation, its inclusive allocated range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentifierAllocation {
    watermark: u64,
    first: Option<u64>,
}

impl IdentifierAllocation {
    pub const fn watermark(self) -> u64 {
        self.watermark
    }

    pub fn range(self) -> Option<RangeInclusive<u64>> {
        self.first.map(|first| first..=self.watermark)
    }
}

impl IdentifierRequest {
    /// Compute the next state while the provider holds exclusive physical admission. Neither observation nor reservation ever lowers an existing watermark; exhaustion leaves it unchanged.
    pub fn prepare(self, current: Option<u64>) -> VersionResult<IdentifierAllocation> {
        let (watermark, first) = match self {
            Self::Observe(value) => (current.map_or(value, |current| current.max(value)), None),
            Self::Reserve {
                minimum,
                maximum,
                count,
            } => {
                if minimum > maximum {
                    return Err(VersionError::InvalidIdentifierBounds);
                }
                let first = match current {
                    Some(current) => current
                        .checked_add(1)
                        .ok_or(VersionError::IdentifiersExhausted)?
                        .max(minimum),
                    None => minimum,
                };
                let last = first
                    .checked_add(count.get() - 1)
                    .filter(|last| *last <= maximum)
                    .ok_or(VersionError::IdentifiersExhausted)?;
                (last, Some(first))
            }
        };
        Ok(IdentifierAllocation { watermark, first })
    }

    /// Validate a request before physical admission and charge the provider's key binding/copy workspace. No identifier is consumed if this fails.
    pub fn reserve_workspace(
        self,
        namespace: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<MemoryReservation> {
        self.prepare(None)?;
        reserve_identifier_workspace(namespace, control)
    }
}

/// Charge namespace bindings for either a reservation or a read without retaining the caller's bytes.
pub fn reserve_identifier_workspace(
    namespace: &[u8],
    control: &StorageReadControl,
) -> VersionResult<MemoryReservation> {
    control.cancellation().check()?;
    if namespace.is_empty() {
        return Err(VersionError::InvalidEncoding("empty identifier namespace"));
    }
    let bytes = namespace
        .len()
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(64))
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(control.memory().reserve(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserve(minimum: u64, maximum: u64, count: u64) -> IdentifierRequest {
        IdentifierRequest::Reserve {
            minimum,
            maximum,
            count: NonZeroU64::new(count).unwrap(),
        }
    }

    #[test]
    fn reservations_cover_zero_full_width_bounds_and_atomic_exhaustion() {
        assert_eq!(reserve(0, 9, 2).prepare(None).unwrap().range(), Some(0..=1));
        assert_eq!(
            reserve(5, 9, 2).prepare(Some(1)).unwrap().range(),
            Some(5..=6)
        );
        assert_eq!(
            reserve(0, u64::MAX, 2)
                .prepare(Some(u64::MAX - 2))
                .unwrap()
                .range(),
            Some(u64::MAX - 1..=u64::MAX)
        );
        for (request, current) in [
            (reserve(0, 9, 2), Some(8)),
            (reserve(0, u64::MAX, 2), Some(u64::MAX - 1)),
            (reserve(0, u64::MAX, 1), Some(u64::MAX)),
        ] {
            assert!(matches!(
                request.prepare(current),
                Err(VersionError::IdentifiersExhausted)
            ));
        }
        assert!(matches!(
            reserve(2, 1, 1).prepare(None),
            Err(VersionError::InvalidIdentifierBounds)
        ));
    }

    #[test]
    fn observations_never_rewind_or_wrap_a_namespace() {
        for (current, observed, expected) in [
            (None, 0, 0),
            (Some(9), 2, 9),
            (Some(2), 9, 9),
            (Some(u64::MAX), 0, u64::MAX),
        ] {
            let allocation = IdentifierRequest::Observe(observed)
                .prepare(current)
                .unwrap();
            assert_eq!(allocation.watermark(), expected);
            assert_eq!(allocation.range(), None);
        }
    }
}
