//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value mutations join the selected catalog session; execution owns autonomous session selection. They address the value record of one definition generation by its object identity and generation, never by name, so that a value operation outside the transaction that renames the sequence finds it.

use super::{
    Catalog, Family, NativeRecordOwner, NativeSnapshot, RelationIdentity, Result, SQLiteError,
};
use rusqlite::types::ValueRef;
use uqa_storage::catalog::{
    sequence_value_reservation, SequenceLogResult, SequenceReservationResult,
    SequenceSetValueResult, SequenceValuePosition,
};

pub(super) fn integer(value: ValueRef<'_>) -> i64 {
    value.as_i64().expect("validated sequence integer column")
}

/// The allocation options of a definition generation, which never change for its life.
struct GenerationOptions {
    increment: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: i64,
}

/// The value state of one definition generation, or why there is none.
enum GenerationValue {
    Found(SequenceValuePosition),
    /// Another generation of the sequence replaced this one.
    Replaced,
    Missing,
}

impl NativeSnapshot {
    fn generation_value(
        &self,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<GenerationValue> {
        // An unassigned identity names no sequence, and an unassigned generation none of the sequence's generations.
        if object_id == [0; 16] {
            return Ok(GenerationValue::Missing);
        }
        if definition_generation != [0; 16] {
            if let Some(position) =
                self.sequence_value(generation_owner(object_id, definition_generation))?
            {
                return Ok(GenerationValue::Found(position));
            }
        }
        let mut replaced = false;
        self.visit_object_rows(Family::Sequences, object_id, |_| {
            replaced = true;
            Ok(())
        })?;
        Ok(if replaced {
            GenerationValue::Replaced
        } else {
            GenerationValue::Missing
        })
    }

    fn generation_options(&self, owner: NativeRecordOwner) -> Result<Option<GenerationOptions>> {
        self.read_row(Family::Sequences, owner, &[], |row| {
            Ok(GenerationOptions {
                increment: integer(row[4]),
                min_value: integer(row[10]),
                max_value: integer(row[11]),
                cycle: integer(row[12]) != 0,
                cache_size: integer(row[13]),
            })
        })
    }
}

const fn generation_owner(
    object_id: [u8; 16],
    definition_generation: [u8; 16],
) -> NativeRecordOwner {
    NativeRecordOwner::Object {
        identity: object_id,
        generation: definition_generation,
    }
}

impl Catalog {
    pub(in crate::catalog) fn reserve_native_sequence_values(
        &self,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<Option<SequenceReservationResult>> {
        let owner = generation_owner(object_id, definition_generation);
        self.conn.with_native_write(|snapshot, batch| {
            let position = match snapshot.generation_value(object_id, definition_generation)? {
                GenerationValue::Found(position) => position,
                GenerationValue::Replaced => {
                    return Ok(SequenceReservationResult::DefinitionChanged)
                }
                GenerationValue::Missing => return Ok(SequenceReservationResult::Missing),
            };
            let options = snapshot.generation_options(owner)?.ok_or_else(|| {
                SQLiteError::StorageBackend(format!(
                    "sequence `{}` has a value record without its definition",
                    relation.qualified_name()
                ))
            })?;
            if options.increment == 0 || options.cache_size <= 0 || position.log_count < 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "corrupt sequence `{}` has increment {}, cache size {} and log count {}",
                    relation.qualified_name(),
                    options.increment,
                    options.cache_size,
                    position.log_count
                )));
            }
            let Some(reservation) = sequence_value_reservation(
                position,
                options.increment,
                options.min_value,
                options.max_value,
                options.cycle,
                options.cache_size,
            ) else {
                return Ok(SequenceReservationResult::Exhausted);
            };
            snapshot.put_sequence_value(
                batch,
                owner,
                SequenceValuePosition {
                    current: reservation.last_value,
                    called: true,
                    log_count: reservation.log_count,
                },
            )?;
            Ok(SequenceReservationResult::Reserved(reservation))
        })
    }

    pub(in crate::catalog) fn set_native_sequence_value(
        &self,
        _relation: &RelationIdentity,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> Result<Option<SequenceSetValueResult>> {
        let owner = generation_owner(object_id, definition_generation);
        self.conn.with_native_write(|snapshot, batch| {
            match snapshot.generation_value(object_id, definition_generation)? {
                GenerationValue::Found(_) => {}
                GenerationValue::Replaced => return Ok(SequenceSetValueResult::DefinitionChanged),
                GenerationValue::Missing => return Ok(SequenceSetValueResult::Missing),
            }
            snapshot.put_sequence_value(
                batch,
                owner,
                SequenceValuePosition {
                    current: value,
                    called,
                    log_count,
                },
            )?;
            Ok(SequenceSetValueResult::Set(value))
        })
    }

    pub(in crate::catalog) fn log_native_sequence_values(
        &self,
        _relation: &RelationIdentity,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        expected: (i64, bool),
        logged: SequenceValuePosition,
    ) -> Result<Option<SequenceLogResult>> {
        let owner = generation_owner(object_id, definition_generation);
        self.conn.with_native_write(|snapshot, batch| {
            let stored = match snapshot.generation_value(object_id, definition_generation)? {
                GenerationValue::Found(position) => position,
                GenerationValue::Replaced => return Ok(SequenceLogResult::DefinitionChanged),
                GenerationValue::Missing => return Ok(SequenceLogResult::Missing),
            };
            if (stored.current, stored.called) != expected {
                return Ok(SequenceLogResult::Changed(stored));
            }
            snapshot.put_sequence_value(batch, owner, logged)?;
            Ok(SequenceLogResult::Logged)
        })
    }
}
