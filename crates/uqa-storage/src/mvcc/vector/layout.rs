//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dispatch physical codecs while keeping vector transaction rules shared.

use super::{IndexKind, Key, Mutation};
use crate::mvcc::{
    CommittedRecordSnapshot, HNSWRecordKey, HNSWRecordLayout, IVFRecordKey, IVFRecordLayout,
    PrivateRecordChanges, VersionError, VersionResult, VersionedPersistence,
};
use crate::{read_control::StorageReadControl, HNSWIndexParams, IVFIndexParams};
use uqa_core::{memory::BudgetedVec, DocId};

#[derive(Clone, Copy)]
pub(in crate::mvcc) enum Layout<'a> {
    IVFIndex(&'a dyn IVFRecordLayout),
    HNSWIndex(&'a dyn HNSWRecordLayout),
}

#[derive(Clone, Copy, PartialEq)]
enum Parameters {
    IVFIndex(IVFIndexParams),
    HNSWIndex(HNSWIndexParams),
}

#[derive(Clone, Copy)]
pub(in crate::mvcc) struct Header {
    dimensions: u32,
    params: Parameters,
    pub(in crate::mvcc) revision: Option<u64>,
}
impl Header {
    pub(in crate::mvcc) fn same_definition(self, other: Self) -> bool {
        self.dimensions == other.dimensions
            && self.params == other.params
            && self.revision.is_some() == other.revision.is_some()
    }
}
impl IndexKind {
    pub(in crate::mvcc) fn layout(
        self,
        persistence: &dyn VersionedPersistence,
    ) -> VersionResult<Layout<'_>> {
        Ok(match self {
            Self::IVFIndex => Layout::IVFIndex(persistence.ivf_record_layout()),
            Self::HNSWIndex => Layout::HNSWIndex(persistence.hnsw_record_layout().ok_or(
                VersionError::InvalidEncoding("provider has no HNSW record layout"),
            )?),
        })
    }
}
impl Layout<'_> {
    pub(in crate::mvcc) fn metadata_key(
        self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>> {
        match self {
            Self::IVFIndex(layout) => layout.metadata_key(key, control),
            Self::HNSWIndex(layout) => layout.metadata_key(key, control),
        }
    }
    pub(in crate::mvcc) fn key(
        self,
        metadata: &[u8],
        address: Key,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        match self {
            Self::IVFIndex(layout) => layout.key(
                metadata,
                match address {
                    Key::Structure => IVFRecordKey::Structure,
                    Key::Document(document) => IVFRecordKey::Document(document),
                    Key::Vectors => IVFRecordKey::Vectors,
                },
                control,
            ),
            Self::HNSWIndex(layout) => layout.key(
                metadata,
                match address {
                    Key::Structure => HNSWRecordKey::Structure,
                    Key::Document(document) => HNSWRecordKey::Document(document),
                    Key::Vectors => HNSWRecordKey::Vectors,
                },
                control,
            ),
        }
    }
    pub(in crate::mvcc) fn header(
        self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Header> {
        Ok(match self {
            Self::IVFIndex(layout) => {
                let header = layout.header(key, value, control)?;
                Header {
                    dimensions: header.dimensions,
                    params: Parameters::IVFIndex(header.params),
                    revision: header.revision,
                }
            }
            Self::HNSWIndex(layout) => {
                let header = layout.header(key, value, control)?;
                Header {
                    dimensions: header.dimensions,
                    params: Parameters::HNSWIndex(header.params),
                    revision: header.revision,
                }
            }
        })
    }
    pub(in crate::mvcc) fn vector_id(
        self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32)> {
        match self {
            Self::IVFIndex(layout) => layout.vector_id(key, control),
            Self::HNSWIndex(layout) => layout.vector_id(key, control),
        }
    }
    pub(in crate::mvcc) fn vector(
        self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)> {
        match self {
            Self::IVFIndex(layout) => layout.vector(key, value, control),
            Self::HNSWIndex(layout) => layout.vector(key, value, control),
        }
    }
    pub(in crate::mvcc) fn merge(
        self,
        key: &[u8],
        operations: &[Mutation<'_>],
        changes: &PrivateRecordChanges,
        current: &dyn CommittedRecordSnapshot,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        match self {
            Self::IVFIndex(layout) => {
                crate::mvcc::ivf::merge(key, operations, changes, current, layout, control)
            }
            Self::HNSWIndex(layout) => {
                crate::mvcc::hnsw::merge(key, operations, changes, current, layout, control)
            }
        }
    }
}
