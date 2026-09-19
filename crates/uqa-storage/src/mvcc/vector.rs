//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared evaluated vector inputs, definition fences and conditional publication.

mod effects;
pub(super) mod layout;
pub(super) mod resolve;
#[cfg(test)]
mod tests;

use super::{commit::RecordWriteKind, VersionError, VersionResult};
use crate::{hnsw_index::HNSWMutation, ivf_index::IVFMutation};
pub(super) use effects::{OwnedVectorMutation, VectorEffects};
pub(super) use resolve::resolve;
use uqa_core::DocId;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum IndexKind {
    IVFIndex,
    HNSWIndex,
}

impl IndexKind {
    pub(super) fn fingerprint_tag(self) -> u8 {
        match self {
            Self::IVFIndex => 0,
            Self::HNSWIndex => 1,
        }
    }
    pub(super) fn preview(self) -> RecordWriteKind {
        match self {
            Self::IVFIndex => RecordWriteKind::IVFPreview,
            Self::HNSWIndex => RecordWriteKind::HNSWPreview,
        }
    }
    pub(super) fn from_preview(kind: RecordWriteKind) -> Option<Self> {
        match kind {
            RecordWriteKind::IVFPreview => Some(Self::IVFIndex),
            RecordWriteKind::HNSWPreview => Some(Self::HNSWIndex),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Mutation<'a> {
    Replace {
        document: DocId,
        vectors: &'a [Vec<f32>],
    },
    Delete(DocId),
}
impl<'a> Mutation<'a> {
    pub(super) fn ivf(self) -> IVFMutation<'a> {
        match self {
            Self::Replace { document, vectors } => IVFMutation::Replace { document, vectors },
            Self::Delete(document) => IVFMutation::Delete(document),
        }
    }
    pub(super) fn hnsw(self) -> HNSWMutation<'a> {
        match self {
            Self::Replace { document, vectors } => HNSWMutation::Replace { document, vectors },
            Self::Delete(document) => HNSWMutation::Delete(document),
        }
    }
}
impl<'a> TryFrom<IVFMutation<'a>> for Mutation<'a> {
    type Error = VersionError;
    fn try_from(value: IVFMutation<'a>) -> VersionResult<Self> {
        match value {
            IVFMutation::Replace { document, vectors } => Ok(Self::Replace { document, vectors }),
            IVFMutation::Delete(document) => Ok(Self::Delete(document)),
            _ => Err(VersionError::InvalidEncoding(
                "structural IVF changes require conditional publication",
            )),
        }
    }
}
impl<'a> TryFrom<HNSWMutation<'a>> for Mutation<'a> {
    type Error = VersionError;
    fn try_from(value: HNSWMutation<'a>) -> VersionResult<Self> {
        match value {
            HNSWMutation::Replace { document, vectors } => Ok(Self::Replace { document, vectors }),
            HNSWMutation::Delete(document) => Ok(Self::Delete(document)),
            HNSWMutation::Clear => Err(VersionError::InvalidEncoding(
                "structural HNSW changes require conditional publication",
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Key {
    Structure,
    Document(DocId),
    Vectors,
}
