//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve sequence names for catalog changes and object identities for value allocation.

use super::{
    text, Family, NativeRecordOwner, NativeSnapshot, RelationIdentity, Result, SQLiteError,
    ValueRef,
};

pub(super) fn row_values<'a>(row: &[ValueRef<'a>]) -> [ValueRef<'a>; 21] {
    row.try_into().expect("validated sequence layout")
}

fn named(row: &[ValueRef<'_>], relation: &RelationIdentity) -> bool {
    row[0] == text(&relation.schema) && row[1] == text(&relation.name)
}

fn owner(row: &[ValueRef<'_>]) -> Result<NativeRecordOwner> {
    let id = |value: ValueRef<'_>| {
        value
            .as_blob()
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| SQLiteError::StorageBackend("invalid native sequence identity".into()))
    };
    Ok(NativeRecordOwner::Object {
        identity: id(row[8])?,
        generation: id(row[14])?,
    })
}

impl NativeSnapshot {
    pub(super) fn sequence_named(
        &self,
        relation: &RelationIdentity,
    ) -> Result<Option<NativeRecordOwner>> {
        let mut found = None;
        self.visit_rows(Family::Sequences, None, &[], |row| {
            if named(row, relation) && found.replace(owner(row)?).is_some() {
                return Err(SQLiteError::StorageBackend(
                    "multiple native sequence definitions share a name".into(),
                ));
            }
            Ok(())
        })?;
        Ok(found)
    }

    pub(super) fn check_sequence_identity(
        &self,
        relation: &RelationIdentity,
        identity: [u8; 16],
    ) -> Result<()> {
        self.visit_object_rows(Family::Sequences, identity, |row| {
            if !named(row, relation) {
                return Err(SQLiteError::StorageBackend(
                    "native sequence object identity belongs to another relation".into(),
                ));
            }
            Ok(())
        })
    }

    pub(super) fn sequence_incarnation(
        &self,
        relation: &RelationIdentity,
        identity: [u8; 16],
    ) -> Result<Option<NativeRecordOwner>> {
        if identity == [0; 16] {
            return Ok(None);
        }
        let mut found = None;
        // Allocation never scans definitions belonging to other sequences, including their ACL payloads.
        self.visit_object_rows(Family::Sequences, identity, |row| {
            if named(row, relation) && found.replace(owner(row)?).is_some() {
                return Err(SQLiteError::StorageBackend(
                    "multiple native sequence generations are active".into(),
                ));
            }
            Ok(())
        })?;
        Ok(found)
    }
}
