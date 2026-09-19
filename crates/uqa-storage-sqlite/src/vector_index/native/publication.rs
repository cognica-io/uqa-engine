//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode canonical vector generations and discardable index previews in the same native batch.

use super::NativeVectorRead;
use crate::mvcc::native::{NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity};
use crate::{Result, SQLiteError};
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

#[derive(Clone, Copy)]
pub(in crate::vector_index) enum VectorPublication {
    Canonical,
    IVFPreview,
    HNSWPreview,
}

impl VectorPublication {
    pub(in crate::vector_index) fn put_row(
        self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        values: &[ValueRef<'_>],
    ) -> Result<()> {
        let owner = read.owner.ok_or_else(|| {
            SQLiteError::StorageBackend("vector publication requires a native table owner".into())
        })?;
        let record = NativeRecord::encode(family, owner, values, &read.snapshot.control)?;
        match self {
            Self::Canonical => batch.put(record.key(), record.row())?,
            Self::IVFPreview => batch.preview_ivf_record(record.key(), Some(record.row()))?,
            Self::HNSWPreview => batch.preview_hnsw_record(record.key(), Some(record.row()))?,
        }
        Ok(())
    }

    pub(in crate::vector_index) fn delete_prefix(
        self,
        read: &NativeVectorRead<'_>,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        components: &[ValueRef<'_>],
    ) -> Result<()> {
        if let Some(owner) = read.owner {
            let prefix = NativeRecordIdentity::new(family, owner)?
                .encode_prefix(components, &read.snapshot.control)?;
            match self {
                Self::Canonical => batch.delete_prefix(&prefix)?,
                Self::IVFPreview => batch.preview_ivf_prefix(&prefix)?,
                Self::HNSWPreview => batch.preview_hnsw_prefix(&prefix)?,
            }
        }
        Ok(())
    }
}
