//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector document and definition fences keep their object/generation identity across lifecycle changes.

use super::{
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot,
};
use crate::Result;
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

// Keep the established table name and family 49 keys so existing tombstone histories retain their identities.
pub(super) const SQL: &str = "CREATE TABLE _uqa_mvcc_native_ivf_guards (table_name TEXT NOT NULL, field TEXT NOT NULL, document_id INTEGER NOT NULL CHECK(document_id >= -1), PRIMARY KEY(table_name, field, document_id)) WITHOUT ROWID";

impl NativeSnapshot {
    pub(crate) fn fence_vector_definitions(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        field: Option<&str>,
    ) -> Result<()> {
        let components = field.map(|field| [ValueRef::Text(field.as_bytes())]);
        for family in [Family::IVFIndexes, Family::HNSWIndexes] {
            let prefix = NativeRecordIdentity::new(family, owner)?.encode_prefix(
                components.as_ref().map_or(&[], |parts| parts.as_slice()),
                &self.control,
            )?;
            match family {
                Family::IVFIndexes => batch.fence_ivf_prefix(&prefix)?,
                Family::HNSWIndexes => batch.fence_hnsw_prefix(&prefix)?,
                _ => unreachable!("vector metadata families"),
            }
        }
        Ok(())
    }
}
