//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value mutations join the selected catalog session; execution owns autonomous session selection.

use super::{
    owner_id, records::row_values, Catalog, Family, RelationIdentity, Result, SQLiteError,
};
use rusqlite::types::ValueRef;
use uqa_storage::catalog::{
    sequence_value_reservation, SequenceReservationResult, SequenceSetValueResult,
    SequenceValuePosition,
};

fn integer(value: ValueRef<'_>) -> i64 {
    value.as_i64().expect("validated sequence integer column")
}

impl Catalog {
    pub(in crate::catalog) fn reserve_native_sequence_values(
        &self,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<Option<SequenceReservationResult>> {
        self.conn.with_native_write(|snapshot, batch| {
            let Some(owner) = snapshot.sequence_incarnation(relation, object_id)? else {
                return Ok(SequenceReservationResult::Missing);
            };
            if owner_id(owner).1 != definition_generation {
                return Ok(SequenceReservationResult::DefinitionChanged);
            }
            let result = snapshot.read_row(Family::Sequences, owner, &[], |row| {
                let increment = integer(row[4]);
                let cache_size = integer(row[13]);
                let log_count = integer(row[20]);
                if increment == 0 || cache_size <= 0 || log_count < 0 {
                    return Err(SQLiteError::StorageBackend(format!(
                        "corrupt sequence `{}` has increment {increment}, cache size {cache_size} and log count {log_count}",
                        relation.qualified_name()
                    )));
                }
                let Some(reservation) = sequence_value_reservation(
                    SequenceValuePosition {
                        current: integer(row[5]),
                        called: integer(row[6]) != 0,
                        log_count,
                    },
                    increment,
                    integer(row[10]),
                    integer(row[11]),
                    integer(row[12]) != 0,
                    cache_size,
                ) else {
                    return Ok(SequenceReservationResult::Exhausted);
                };
                let mut updated = row_values(row);
                updated[5] = ValueRef::Integer(reservation.last_value);
                updated[6] = ValueRef::Integer(1);
                updated[20] = ValueRef::Integer(reservation.log_count);
                snapshot.put_row(batch, Family::Sequences, owner, &updated)?;
                Ok(SequenceReservationResult::Reserved(reservation))
            })?;
            Ok(result.expect("resolved sequence on retained snapshot"))
        })
    }

    pub(in crate::catalog) fn set_native_sequence_value(
        &self,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> Result<Option<SequenceSetValueResult>> {
        self.conn.with_native_write(|snapshot, batch| {
            let Some(owner) = snapshot.sequence_incarnation(relation, object_id)? else {
                return Ok(SequenceSetValueResult::Missing);
            };
            if owner_id(owner).1 != definition_generation {
                return Ok(SequenceSetValueResult::DefinitionChanged);
            }
            if log_count < 0 {
                return Err(SQLiteError::StorageBackend(
                    "sequence log count cannot be negative".into(),
                ));
            }
            let updated = snapshot.read_row(Family::Sequences, owner, &[], |row| {
                let mut updated = row_values(row);
                updated[5] = ValueRef::Integer(value);
                updated[6] = ValueRef::Integer(i64::from(called));
                updated[20] = ValueRef::Integer(log_count);
                snapshot.put_row(batch, Family::Sequences, owner, &updated)?;
                Ok(SequenceSetValueResult::Set(value))
            })?;
            Ok(updated.expect("resolved sequence on retained snapshot"))
        })
    }
}
