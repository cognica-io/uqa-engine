//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-owned rows move by their declared native layouts without losing storage classes or index namespaces.

mod merge;

use super::{text, Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError};
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, KeyValueBatch};

impl NativeSnapshot {
    pub(in crate::catalog::native) fn check_table_identity(
        &self,
        name: &str,
        identity: [u8; 16],
    ) -> Result<()> {
        self.visit_rows(
            Family::TableOwners,
            Some(NativeRecordOwner::Database(self.database)),
            &[],
            |row| {
                if row[0] != text(name) && row[1] == ValueRef::Blob(&identity) {
                    return Err(SQLiteError::StorageBackend(
                        "native table object identity already belongs to another name".into(),
                    ));
                }
                Ok(())
            },
        )
    }

    pub(in crate::catalog::native) fn put_table_binding(
        &self,
        batch: &mut dyn KeyValueBatch,
        name: &str,
        owner: NativeRecordOwner,
        catalog: bool,
    ) -> Result<()> {
        let NativeRecordOwner::Object {
            identity,
            generation,
        } = owner
        else {
            return Err(SQLiteError::StorageBackend(
                "native table requires an object owner".into(),
            ));
        };
        self.put_row(
            batch,
            Family::TableOwners,
            NativeRecordOwner::Database(self.database),
            &[
                text(name),
                ValueRef::Blob(&identity),
                ValueRef::Blob(&generation),
                ValueRef::Integer(i64::from(catalog)),
            ],
        )
    }

    pub(in crate::catalog::native) fn transfer_table_data(
        &self,
        batch: &mut dyn KeyValueBatch,
        from: NativeRecordOwner,
        to: NativeRecordOwner,
        name: &str,
    ) -> Result<()> {
        self.reset_occurrence_rows(batch, from)?;
        self.fence_ivf_definitions(batch, from, None)?;
        if from != to {
            self.reset_occurrence_rows(batch, to)?;
            self.fence_ivf_definitions(batch, to, None)?;
        }
        for family in Family::all() {
            let Some(column) = family
                .layout()
                .columns
                .iter()
                .position(|name| *name == "table_name")
            else {
                continue;
            };
            self.visit_rows(family, Some(from), &[], |row| {
                let mut updated = BudgetedVec::new(self.control.memory());
                updated.extend_from_slice(row).map_err(VersionError::from)?;
                updated[column] = text(name);
                self.put_row(batch, family, to, &updated)
            })?;
            if from != to {
                self.delete_prefix(batch, family, from, &[])?;
            }
        }
        Ok(())
    }

    pub(in crate::catalog::native) fn clear_table_data(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        analyzers: bool,
    ) -> Result<()> {
        self.reset_occurrence_rows(batch, owner)?;
        self.fence_ivf_definitions(batch, owner, None)?;
        for family in Family::all() {
            if family.layout().columns.contains(&"table_name")
                && (analyzers || family != Family::TableFieldAnalyzers)
            {
                self.delete_prefix(batch, family, owner, &[])?;
            }
        }
        Ok(())
    }
}
