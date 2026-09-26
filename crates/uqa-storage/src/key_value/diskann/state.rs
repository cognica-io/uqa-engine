//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::read_control::{StorageReadControl, ValueReadVisitor};
use crate::StorageBackendResult;

use super::invalid;

pub(super) const STATE_BYTES: usize = 18;

/// Physical generation lifecycle. A seal alone does not select a catalog head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskANNStageStatus {
    Writing,
    Frozen,
    Sealed,
    Discarding,
    Published,
    Retired,
}

impl DiskANNStageStatus {
    pub(super) fn is_complete(self) -> bool {
        matches!(self, Self::Sealed | Self::Published | Self::Retired)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StageOwner {
    Legacy([u8; 16]),
    Leased(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct State {
    pub(super) status: DiskANNStageStatus,
    pub(super) owner: StageOwner,
}

impl State {
    pub(super) fn encode(self) -> [u8; STATE_BYTES] {
        let mut bytes = [0; STATE_BYTES];
        match self.owner {
            StageOwner::Legacy(owner) => {
                bytes[0] = 1;
                bytes[2..].copy_from_slice(&owner);
            }
            StageOwner::Leased(allocation) => {
                bytes[0] = 2;
                bytes[10..].copy_from_slice(&allocation.to_be_bytes());
            }
        }
        bytes[1] = match self.status {
            DiskANNStageStatus::Writing => 0,
            DiskANNStageStatus::Frozen => 1,
            DiskANNStageStatus::Sealed => 2,
            DiskANNStageStatus::Discarding => 3,
            DiskANNStageStatus::Published => 4,
            DiskANNStageStatus::Retired => 5,
        };
        bytes
    }

    pub(super) fn decode(bytes: [u8; STATE_BYTES]) -> StorageBackendResult<Self> {
        let owner = match bytes[0] {
            1 if bytes[2..] != [0; 16] => {
                StageOwner::Legacy(bytes[2..].try_into().expect("fixed staging identity"))
            }
            2 if bytes[2..10] == [0; 8] => {
                let allocation =
                    u64::from_be_bytes(bytes[10..].try_into().expect("fixed owner allocation"));
                if !(2..=u64::MAX / 2).contains(&allocation) {
                    return Err(invalid("invalid staging owner allocation"));
                }
                StageOwner::Leased(allocation)
            }
            _ => return Err(invalid("invalid staging revision or owner")),
        };
        let status = match bytes[1] {
            0 => DiskANNStageStatus::Writing,
            1 => DiskANNStageStatus::Frozen,
            2 => DiskANNStageStatus::Sealed,
            3 => DiskANNStageStatus::Discarding,
            4 => DiskANNStageStatus::Published,
            5 => DiskANNStageStatus::Retired,
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
    fn diskann_owner_encoding_rejects_reserved_and_overflowed_resource_tags() {
        for allocation in [0, 1, u64::MAX / 2 + 1, u64::MAX] {
            let encoded = State {
                status: DiskANNStageStatus::Writing,
                owner: StageOwner::Leased(allocation),
            }
            .encode();
            assert!(State::decode(encoded).is_err());
        }
        let valid = State {
            status: DiskANNStageStatus::Sealed,
            owner: StageOwner::Leased(u64::MAX / 2),
        }
        .encode();
        assert!(State::decode(valid).is_ok());
        let mut corrupt = valid;
        corrupt[2] = 1;
        assert!(State::decode(corrupt).is_err());
    }

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
