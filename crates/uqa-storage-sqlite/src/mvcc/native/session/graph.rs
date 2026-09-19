//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stage a source row and its changed versioned selectors in one caller-owned batch.

use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

use super::{Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot, Result};
use crate::mvcc::{native::graph_lookup, Error};

impl NativeSnapshot {
    pub(crate) fn replace_graph_row(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        components: &[ValueRef<'_>],
        row: Option<&[ValueRef<'_>]>,
        cache: bool,
    ) -> Result<()> {
        self.guard_graph_row_lifetimes(batch, family, components, row)?;
        let owner = NativeRecordOwner::Database(self.database);
        let old = self
            .read_row(family, owner, components, |values| {
                graph_lookup::records(self.database, family, values, &self.control)
                    .map_err(Error::into_version)
                    .map_err(Into::into)
            })?
            .unwrap_or([None, None, None]);
        let new = row
            .map(|values| {
                graph_lookup::records(self.database, family, values, &self.control)
                    .map_err(Error::into_version)
            })
            .transpose()?
            .unwrap_or([None, None, None]);
        for previous in old.iter().flatten() {
            if !new
                .iter()
                .flatten()
                .any(|next| next.key() == previous.key())
            {
                batch.delete(previous.key())?;
            }
        }
        for next in new.iter().flatten() {
            // A completed path build writes its binding even when unchanged; source-only cache invalidations leave that binding revision untouched.
            if cache
                || !old
                    .iter()
                    .flatten()
                    .any(|previous| previous.key() == next.key())
            {
                batch.put(next.key(), next.row())?;
            }
        }
        if let Some(row) = row {
            let record = super::super::NativeRecord::encode(family, owner, row, &self.control)?;
            if cache {
                batch.replace_graph_cache(record.key(), Some(record.row()))?;
            } else {
                batch.put(record.key(), record.row())?;
            }
        } else {
            let key =
                NativeRecordIdentity::new(family, owner)?.encode_key(components, &self.control)?;
            if cache {
                batch.replace_graph_cache(&key, None)?;
            } else {
                batch.delete(&key)?;
            }
        }
        Ok(())
    }
}
