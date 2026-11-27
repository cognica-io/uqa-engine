//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each probe retains its own position through resident or spilled duplicate build rows.

use crate::spill::DiskPositions;
use crate::ExecResult;
use std::sync::Arc;

pub(crate) enum HashMatches {
    Empty,
    Memory {
        indices: Arc<Vec<u64>>,
        position: usize,
    },
    Disk(DiskPositions),
}

impl HashMatches {
    pub(in crate::join) fn empty() -> Self {
        Self::Empty
    }

    pub(in crate::join) fn memory(indices: Arc<Vec<u64>>) -> Self {
        Self::Memory {
            indices,
            position: 0,
        }
    }

    pub(in crate::join) fn disk(positions: DiskPositions) -> Self {
        Self::Disk(positions)
    }

    pub(crate) fn next(&mut self) -> ExecResult<Option<u64>> {
        match self {
            Self::Empty => Ok(None),
            Self::Memory { indices, position } => {
                let row = indices.get(*position).copied();
                if row.is_some() {
                    *position += 1;
                }
                Ok(row)
            }
            Self::Disk(positions) => positions.next(),
        }
    }
}
