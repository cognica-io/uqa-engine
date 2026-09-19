//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog relation claims and their definitions share one evaluated native mutation batch.

use super::{
    string, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError,
};
use crate::catalog::{RelationIdentity, RelationKind};
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

#[derive(Clone, Copy)]
pub(in crate::catalog) enum RelationRecord {
    View,
    ForeignTable,
}

impl RelationRecord {
    fn family(self) -> Family {
        match self {
            Self::View => Family::Views,
            Self::ForeignTable => Family::ForeignTables,
        }
    }

    fn kind(self) -> RelationKind {
        match self {
            Self::View => RelationKind::View,
            Self::ForeignTable => RelationKind::ForeignTable,
        }
    }
}

impl NativeSnapshot {
    pub(in crate::catalog) fn relation_kind(
        &self,
        relation: &RelationIdentity,
    ) -> Result<Option<String>> {
        self.read_row(
            Family::Relations,
            NativeRecordOwner::Database(self.database),
            &[text(&relation.schema), text(&relation.name)],
            |row| string(row[2]),
        )
    }

    pub(in crate::catalog) fn claim_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let owner = NativeRecordOwner::Database(self.database);
        let schema_exists = self.contains_row(Family::Schemas, owner, &[text(&relation.schema)])?;
        let existing = self.relation_kind(relation)?;
        if Catalog::check_relation_claim(relation, kind, schema_exists, existing.as_deref())? {
            self.put_row(
                batch,
                Family::Relations,
                owner,
                &[
                    text(&relation.schema),
                    text(&relation.name),
                    text(kind.as_str()),
                ],
            )?;
        }
        Ok(())
    }

    pub(in crate::catalog) fn release_relation(
        &self,
        batch: &mut dyn KeyValueBatch,
        relation: &RelationIdentity,
        kind: RelationKind,
    ) -> Result<()> {
        let existing = self.relation_kind(relation)?;
        if Catalog::check_relation_release(relation, kind, existing.as_deref())? {
            self.delete_prefix(
                batch,
                Family::Relations,
                NativeRecordOwner::Database(self.database),
                &[text(&relation.schema), text(&relation.name)],
            )?;
        }
        Ok(())
    }
}

impl Catalog {
    pub(in crate::catalog) fn save_native_relation(
        &self,
        record: RelationRecord,
        relation: &RelationIdentity,
        values: &[ValueRef<'_>],
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.claim_relation(batch, relation, record.kind())?;
            snapshot.clear_relation_acls(batch, relation)?;
            snapshot.put_row(
                batch,
                record.family(),
                NativeRecordOwner::Database(snapshot.database),
                values,
            )
        })
    }

    pub(in crate::catalog) fn rename_native_relation(
        &self,
        record: RelationRecord,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            let key = [text(&from.schema), text(&from.name)];
            let exists = snapshot.contains_row(record.family(), owner, &key)?;
            if from == to || !exists {
                return Ok(exists);
            }
            if snapshot.relation_kind(to)?.is_some() {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation `{}` already exists",
                    to.qualified_name()
                )));
            }
            snapshot.claim_relation(batch, to, record.kind())?;
            snapshot.rename_relation_acls(batch, from, to)?;
            snapshot.read_row(record.family(), owner, &key, |row| {
                // Only the two name columns change; every payload and nullable ACL stays intact.
                let mut renamed = uqa_core::memory::BudgetedVec::new(snapshot.control.memory());
                for (index, value) in row.iter().enumerate() {
                    let value = match index {
                        0 => text(&to.schema),
                        1 => text(&to.name),
                        _ => *value,
                    };
                    renamed
                        .push(value)
                        .map_err(uqa_storage::mvcc::VersionError::from)?;
                }
                snapshot.put_row(batch, record.family(), owner, &renamed)
            })?;
            snapshot.delete_prefix(batch, record.family(), owner, &key)?;
            snapshot.release_relation(batch, from, record.kind())?;
            Ok(true)
        })
    }

    pub(in crate::catalog) fn drop_native_relation(
        &self,
        record: RelationRecord,
        relation: &RelationIdentity,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            let key = [text(&relation.schema), text(&relation.name)];
            let exists = snapshot.contains_row(record.family(), owner, &key)?;
            if exists {
                snapshot.clear_relation_acls(batch, relation)?;
                snapshot.delete_prefix(batch, record.family(), owner, &key)?;
                snapshot.release_relation(batch, relation, record.kind())?;
            }
            Ok(exists)
        })
    }
}
