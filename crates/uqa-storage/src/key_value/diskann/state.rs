//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::read_control::{StorageReadControl, ValueReadVisitor};
use crate::StorageBackendResult;

use super::invalid;

pub(super) const STATE_BYTES: usize = 18;

/// Physical staging status. A sealed generation is not a public catalog publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskANNStageStatus {
    Writing,
    Frozen,
    Sealed,
    Discarding,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct State {
    pub(super) status: DiskANNStageStatus,
    pub(super) owner: [u8; 16],
}

impl State {
    pub(super) fn encode(self) -> [u8; STATE_BYTES] {
        let mut bytes = [0; STATE_BYTES];
        bytes[0] = 1;
        bytes[1] = match self.status {
            DiskANNStageStatus::Writing => 0,
            DiskANNStageStatus::Frozen => 1,
            DiskANNStageStatus::Sealed => 2,
            DiskANNStageStatus::Discarding => 3,
        };
        bytes[2..].copy_from_slice(&self.owner);
        bytes
    }

    pub(super) fn decode(bytes: [u8; STATE_BYTES]) -> StorageBackendResult<Self> {
        let owner = bytes[2..].try_into().expect("fixed staging identity");
        if bytes[0] != 1 || owner == [0; 16] {
            return Err(invalid("invalid staging revision or owner"));
        }
        let status = match bytes[1] {
            0 => DiskANNStageStatus::Writing,
            1 => DiskANNStageStatus::Frozen,
            2 => DiskANNStageStatus::Sealed,
            3 => DiskANNStageStatus::Discarding,
            _ => return Err(invalid("unknown staging status")),
        };
        Ok(Self { status, owner })
    }
}

pub(super) fn data_identity(bytes: [u8; 17]) -> StorageBackendResult<[u8; 16]> {
    let identity = bytes[1..].try_into().expect("fixed data identity");
    if bytes[0] != 1 || identity == [0; 16] {
        return Err(invalid("invalid data identity or revision"));
    }
    Ok(identity)
}

pub(super) fn fixed<const N: usize>(
    control: &StorageReadControl,
    read: impl FnOnce(&mut ValueReadVisitor<'_>) -> StorageBackendResult<()>,
) -> StorageBackendResult<Option<[u8; N]>> {
    control.check()?;
    let mut result = None;
    let mut seen = false;
    let mut failed = false;
    let source = read(&mut |value| {
        if seen || failed {
            failed = true;
            return Err(invalid("metadata was returned more than once"));
        }
        seen = true;
        if let Some(value) = value {
            let Ok(bytes) = value.try_into() else {
                failed = true;
                return Err(invalid("metadata size differs"));
            };
            result = Some(bytes);
        }
        Ok(())
    });
    if failed {
        return Err(invalid("incomplete or invalid metadata completion"));
    }
    source?;
    if !seen {
        return Err(invalid("metadata was not returned"));
    }
    control.check()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StorageBackendError;
    use uqa_core::memory::MemoryError;

    #[test]
    fn diskann_metadata_preserves_provider_quota_before_any_completion() {
        let control = StorageReadControl::with_limit(64);
        let error = fixed::<STATE_BYTES>(&control, |_| {
            Err(MemoryError::Limit {
                required: 128,
                limit: 64,
            }
            .into())
        })
        .unwrap_err();
        assert!(matches!(
            error,
            StorageBackendError::Memory(MemoryError::Limit {
                required: 128,
                limit: 64
            })
        ));
    }

    #[test]
    fn diskann_metadata_rejects_missing_repeated_or_swallowed_invalid_completion() {
        let control = StorageReadControl::with_limit(64);
        assert!(fixed::<STATE_BYTES>(&control, |_| Ok(())).is_err());
        assert!(fixed::<STATE_BYTES>(&control, |visit| {
            visit(None)?;
            let _ = visit(None);
            Ok(())
        })
        .is_err());
        assert!(fixed::<STATE_BYTES>(&control, |visit| {
            let _ = visit(Some(&[1]));
            Ok(())
        })
        .is_err());
        assert_eq!(
            fixed::<STATE_BYTES>(&control, |visit| visit(None)).unwrap(),
            None
        );
    }
}
