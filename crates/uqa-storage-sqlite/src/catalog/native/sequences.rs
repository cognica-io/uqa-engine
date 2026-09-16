//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence definitions and relation claims share the native catalog session.

mod records;
mod values;

use super::{
    optional_text, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError,
};
use crate::catalog::{
    sequences_views::codec::{concrete_sequence_options, decode_native_sequence_row},
    RelationIdentity, RelationKind, SequenceRow,
};
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

fn owner_id(owner: NativeRecordOwner) -> ([u8; 16], [u8; 16]) {
    let NativeRecordOwner::Object {
        identity,
        generation,
    } = owner
    else {
        unreachable!("sequence definitions are object-owned")
    };
    (identity, generation)
}

impl Catalog {
    pub(in crate::catalog) fn write_native_sequence(
        &self,
        sequence: &SequenceRow,
        replace: bool,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            let previous = snapshot.sequence_named(&sequence.relation)?;
            if replace != previous.is_some() {
                return Ok(false);
            }
            let (old_id, old_generation) = previous.map_or(([0; 16], [0; 16]), owner_id);
            let identity = snapshot.allocate_identity(if sequence.object_id == [0; 16] {
                old_id
            } else {
                sequence.object_id
            })?;
            let generation =
                snapshot.allocate_identity(if sequence.definition_generation == [0; 16] {
                    old_generation
                } else {
                    sequence.definition_generation
                })?;
            let owner = NativeRecordOwner::Object {
                identity,
                generation,
            };
            snapshot.check_sequence_identity(&sequence.relation, identity)?;
            if !replace {
                snapshot.claim_relation(batch, &sequence.relation, RelationKind::Sequence)?;
            }
            if let Some(old) = previous.filter(|old| *old != owner) {
                snapshot.delete_prefix(batch, Family::Sequences, old, &[])?;
            }
            snapshot.put_sequence(batch, sequence, owner)?;
            Ok(true)
        })
    }

    pub(in crate::catalog) fn rename_native_sequence(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            let Some(owner) = snapshot.sequence_named(from)? else {
                return Ok(false);
            };
            if from == to {
                return Ok(true);
            }
            if snapshot.relation_kind(to)?.is_some() {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation `{}` already exists",
                    to.qualified_name()
                )));
            }
            snapshot.claim_relation(batch, to, RelationKind::Sequence)?;
            snapshot.read_row(Family::Sequences, owner, &[], |row| {
                let mut renamed = records::row_values(row);
                renamed[0] = text(&to.schema);
                renamed[1] = text(&to.name);
                snapshot.put_row(batch, Family::Sequences, owner, &renamed)
            })?;
            snapshot.release_relation(batch, from, RelationKind::Sequence)?;
            Ok(true)
        })
    }

    pub(in crate::catalog) fn drop_native_sequence(
        &self,
        relation: &RelationIdentity,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            let Some(owner) = snapshot.sequence_named(relation)? else {
                return Ok(false);
            };
            snapshot.delete_prefix(batch, Family::Sequences, owner, &[])?;
            snapshot.release_relation(batch, relation, RelationKind::Sequence)?;
            Ok(true)
        })
    }

    pub(in crate::catalog) fn load_native_sequences(&self) -> Result<Option<Vec<SequenceRow>>> {
        self.read_native(|snapshot| {
            let mut sequences = Vec::new();
            snapshot.visit_rows(Family::Sequences, None, &[], |row| {
                sequences.push(decode_native_sequence_row(row)?);
                Ok(())
            })?;
            sequences.sort_unstable_by(|left, right| left.relation.cmp(&right.relation));
            Ok(sequences)
        })
    }
}

impl NativeSnapshot {
    fn put_sequence(
        &self,
        batch: &mut dyn KeyValueBatch,
        sequence: &SequenceRow,
        owner: NativeRecordOwner,
    ) -> Result<()> {
        // Match the physical catalog checks before adding any mutation to the caller's transaction.
        if !matches!(sequence.persistence.as_str(), "p" | "u")
            || sequence.options.cache_size <= 0
            || sequence.log_count < 0
        {
            return Err(SQLiteError::StorageBackend(format!(
                "invalid durable sequence `{}` persistence, cache size or log count",
                sequence.relation.qualified_name()
            )));
        }
        let (identity, generation) = owner_id(owner);
        let options = concrete_sequence_options(sequence);
        let acl = sequence
            .acl
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let owner_table = sequence.owner.map(|owner| owner.table_object_id);
        let owner_column = sequence.owner.map(|owner| owner.column_object_id);
        self.put_row(
            batch,
            Family::Sequences,
            owner,
            &[
                text(&sequence.relation.schema),
                text(&sequence.relation.name),
                text("sequence"),
                ValueRef::Integer(sequence.start),
                ValueRef::Integer(sequence.increment),
                ValueRef::Integer(sequence.current),
                ValueRef::Integer(i64::from(sequence.called)),
                text(&sequence.persistence),
                ValueRef::Blob(&identity),
                text(&options.data_type),
                ValueRef::Integer(options.min_value.expect("concrete bound")),
                ValueRef::Integer(options.max_value.expect("concrete bound")),
                ValueRef::Integer(i64::from(options.cycle)),
                ValueRef::Integer(options.cache_size),
                ValueRef::Blob(&generation),
                owner_table
                    .as_ref()
                    .map_or(ValueRef::Null, |id| ValueRef::Blob(id)),
                owner_column
                    .as_ref()
                    .map_or(ValueRef::Null, |id| ValueRef::Blob(id)),
                optional_text(sequence.owner.map(|owner| owner.dependency.catalog_code())),
                text(&sequence.role_owner),
                optional_text(acl.as_deref()),
                ValueRef::Integer(sequence.log_count),
            ],
        )
    }
}
