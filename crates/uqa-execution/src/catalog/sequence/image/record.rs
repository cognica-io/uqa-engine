//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed native metadata retains integer boundaries and exact allocation incarnations.

use super::{invalid, CapturedSequence};
use crate::catalog::sequence::{registry::SequenceRecord, SequenceState};
use uqa_core::catalog_sequence::{SequenceOwner, SequenceOwnerDependency};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    SQLError,
};

pub(in super::super) const RECORD_BYTES: usize = 130;

struct Wire {
    bytes: [u8; RECORD_BYTES],
    position: usize,
}

impl Wire {
    fn put<const N: usize>(&mut self, value: [u8; N]) {
        self.bytes[self.position..self.position + N].copy_from_slice(&value);
        self.position += N;
    }
    fn get<const N: usize>(&mut self) -> [u8; N] {
        let result = self.bytes[self.position..self.position + N]
            .try_into()
            .unwrap();
        self.position += N;
        result
    }
    fn flag(&mut self) -> Result<bool, SQLError> {
        match self.get::<1>()[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("boolean tag")),
        }
    }
    fn number(&mut self) -> i64 {
        i64::from_le_bytes(self.get())
    }
}

pub(super) fn encode(entry: &CapturedSequence<'_>) -> Result<[u8; RECORD_BYTES], SQLError> {
    let mut wire = Wire {
        bytes: [0; RECORD_BYTES],
        position: 0,
    };
    wire.put(entry.object_id);
    wire.put([u8::from(entry.catalog_oid.is_some())]);
    wire.put(entry.catalog_oid.unwrap_or(0).to_le_bytes());
    wire.put([match entry.persistence {
        RelationPersistence::Permanent => 0,
        RelationPersistence::Unlogged => 1,
        RelationPersistence::Temporary => return Err(invalid("temporary sequence")),
    }]);
    let state = entry.state;
    wire.put([match state.data_type {
        SequenceDataType::SmallInt => 0,
        SequenceDataType::Integer => 1,
        SequenceDataType::BigInt => 2,
    }]);
    wire.put([u8::from(state.called)]);
    wire.put([u8::from(state.cycle)]);
    let (dependency, table, column) = match state.owner {
        None => (0, [0; 16], [0; 16]),
        Some(owner) => (
            match owner.dependency {
                SequenceOwnerDependency::Automatic => 1,
                SequenceOwnerDependency::Internal => 2,
            },
            owner.table_object_id,
            owner.column_object_id,
        ),
    };
    wire.put([dependency]);
    wire.put(table);
    wire.put(column);
    wire.put(state.definition_generation);
    for number in [
        state.start,
        state.increment,
        state.current,
        state.log_count,
        state.min_value,
        state.max_value,
        state.cache_size,
    ] {
        wire.put(number.to_le_bytes());
    }
    debug_assert_eq!(wire.position, RECORD_BYTES);
    Ok(wire.bytes)
}

pub(in super::super) fn decode(bytes: [u8; RECORD_BYTES]) -> Result<SequenceRecord, SQLError> {
    let mut wire = Wire { bytes, position: 0 };
    let object_id = wire.get();
    let has_oid = wire.flag()?;
    let oid = u32::from_le_bytes(wire.get());
    if object_id == [0; 16]
        || (!has_oid && oid != 0)
        || (has_oid && oid < uqa_sql::catalog::oids::FIRST_NORMAL_OBJECT_ID)
    {
        return Err(invalid("object identity"));
    }
    let persistence = match wire.get::<1>()[0] {
        0 => RelationPersistence::Permanent,
        1 => RelationPersistence::Unlogged,
        _ => return Err(invalid("persistence")),
    };
    let data_type = match wire.get::<1>()[0] {
        0 => SequenceDataType::SmallInt,
        1 => SequenceDataType::Integer,
        2 => SequenceDataType::BigInt,
        _ => return Err(invalid("data type")),
    };
    let called = wire.flag()?;
    let cycle = wire.flag()?;
    let dependency = wire.get::<1>()[0];
    let table_object_id = wire.get();
    let column_object_id = wire.get();
    let owner = match dependency {
        0 if table_object_id == [0; 16] && column_object_id == [0; 16] => None,
        1 | 2 if table_object_id != [0; 16] && column_object_id != [0; 16] => Some(SequenceOwner {
            table_object_id,
            column_object_id,
            dependency: if dependency == 1 {
                SequenceOwnerDependency::Automatic
            } else {
                SequenceOwnerDependency::Internal
            },
        }),
        _ => return Err(invalid("owner identity")),
    };
    let definition_generation = wire.get();
    let state = SequenceState {
        definition_generation,
        start: wire.number(),
        increment: wire.number(),
        current: wire.number(),
        log_count: wire.number(),
        min_value: wire.number(),
        max_value: wire.number(),
        cache_size: wire.number(),
        data_type,
        called,
        cycle,
        owner,
    };
    if definition_generation == [0; 16] || state.log_count < 0 {
        return Err(invalid("allocation generation"));
    }
    // Catalog restoration deliberately permits legacy positions outside current bounds; changing the position would reuse or skip values.
    uqa_sql::schema::sequences::definition::validate_sequence_definition(&state.definition(), None)
        .map_err(|_| invalid("sequence definition"))?;
    debug_assert_eq!(wire.position, RECORD_BYTES);
    Ok(SequenceRecord {
        state,
        object_id,
        persistence,
        catalog_oid: has_oid.then_some(oid),
    })
}
