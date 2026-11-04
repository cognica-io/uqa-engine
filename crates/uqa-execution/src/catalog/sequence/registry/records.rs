//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable sequence records retain definition, position, identity and actual persistence together.

use super::super::SequenceState;
use std::iter::Peekable;
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, MemoryReservation, PreparedRetainedMapInsert, Produced,
        ProductionControl, RetainedMap, RetainedMapBidirectionalIter, RetainedMapSnapshot,
    },
    CancellationToken, RelationIdentity,
};
use uqa_sql::{ast::RelationPersistence, SQLError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceRecord {
    pub state: SequenceState,
    pub object_id: [u8; 16],
    pub persistence: RelationPersistence,
    pub catalog_oid: Option<u32>,
}

#[derive(Clone)]
pub(super) struct SequenceRecords {
    durable: RetainedMap<RelationIdentity, Budgeted<SequenceRecord>>,
    temporary: RetainedMap<RelationIdentity, Budgeted<SequenceRecord>>,
}

impl SequenceRecords {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            durable: RetainedMap::new(memory),
            temporary: RetainedMap::new(memory),
        }
    }
    pub(super) fn shares_roots(&self, other: &Self) -> bool {
        self.durable.shares_root(&other.durable) && self.temporary.shares_root(&other.temporary)
    }
    pub(super) fn budget(&self) -> &MemoryBudget {
        self.durable.budget()
    }
    pub(super) fn temporary_only(&self) -> Self {
        Self {
            durable: RetainedMap::new(self.budget()),
            temporary: self.temporary.clone(),
        }
    }
    pub(super) fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
    }
    pub(super) fn get(&self, relation: &RelationIdentity) -> Option<&SequenceRecord> {
        self.durable
            .get(relation)
            .or_else(|| self.temporary.get(relation))
            .map(|row| &**row)
    }
    pub(super) fn iter(&self) -> SequenceRecordIter<'_> {
        SequenceRecordIter {
            durable: self.durable.iter_bidirectional().peekable(),
            temporary: self.temporary.iter_bidirectional().peekable(),
        }
    }

    fn admit(
        &self,
        relation: &RelationIdentity,
        record: SequenceRecord,
    ) -> Result<(RelationIdentity, Budgeted<SequenceRecord>), SQLError> {
        let token = CancellationToken::new();
        let control = ProductionControl::new(self.budget(), &token, &token);
        let (schema, schema_memory) = control.copy_text(&relation.schema)?.into_parts();
        let (name, name_memory) = control.copy_text(&relation.name)?.into_parts();
        let record = control
            .finish(record, control.combine(schema_memory, name_memory))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native sequence records retain their allowance"));
        Ok((RelationIdentity { schema, name }, record))
    }

    pub(super) fn insert(
        &mut self,
        relation: &RelationIdentity,
        record: SequenceRecord,
    ) -> Result<(), SQLError> {
        let (relation, record) = self.admit(relation, record)?;
        let bytes = record.reserved_bytes();
        let mut next = self.clone();
        if record.persistence == RelationPersistence::Temporary {
            next.durable.remove(&relation)?;
            next.temporary.insert(relation, record, bytes)?;
        } else {
            next.temporary.remove(&relation)?;
            next.durable.insert(relation, record, bytes)?;
        }
        *self = next;
        Ok(())
    }

    pub(super) fn remove(&mut self, relation: &RelationIdentity) -> Result<bool, SQLError> {
        let mut next = self.clone();
        let durable = next.durable.remove(relation)?;
        let temporary = next.temporary.remove(relation)?;
        *self = next;
        Ok(durable || temporary)
    }

    pub(super) fn prepare_position(
        &mut self,
        relation: &RelationIdentity,
        object: [u8; 16],
        definition: [u8; 16],
    ) -> Result<Option<PreparedSequencePosition<'_>>, SQLError> {
        let Some(record) = self.get(relation).copied() else {
            return Ok(None);
        };
        if record.object_id != object || record.state.definition_generation != definition {
            return Ok(None);
        }
        let (key, value) = self.admit(relation, record)?;
        let bytes = value.reserved_bytes();
        let (record, memory) = value.into_parts();
        let selected = if record.persistence == RelationPersistence::Temporary {
            &mut self.temporary
        } else {
            &mut self.durable
        };
        Ok(Some(PreparedSequencePosition {
            insertion: selected.prepare_insert(key, bytes)?,
            record,
            memory,
        }))
    }

    pub(super) fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedSequenceRecords>, SQLError> {
        control.check()?;
        let memory = control
            .budget()
            .ok_or_else(|| SQLError::Internal("sequence capture requires admission".into()))?;
        let (root, memory) = self.durable.capture(memory)?.into_parts();
        Ok(control.finish(CapturedSequenceRecords { root }, Some(memory))?)
    }
}

pub struct PreparedSequencePosition<'a> {
    insertion: PreparedRetainedMapInsert<'a, RelationIdentity, Budgeted<SequenceRecord>>,
    record: SequenceRecord,
    memory: MemoryReservation,
}

impl PreparedSequencePosition<'_> {
    pub fn state(&self) -> SequenceState {
        self.record.state
    }

    /// Publish the externally selected position under the complete preceding admission. Definition, identity, authority membership and payload capacities remain unchanged.
    pub fn publish(mut self, position: uqa_storage::SequenceValuePosition) {
        self.record.state.current = position.current;
        self.record.state.called = position.called;
        self.record.state.log_count = position.log_count;
        self.insertion
            .publish(Budgeted::new(self.record, self.memory));
    }
}

pub struct SequenceRecordIter<'a> {
    durable: Peekable<RetainedMapBidirectionalIter<'a, RelationIdentity, Budgeted<SequenceRecord>>>,
    temporary:
        Peekable<RetainedMapBidirectionalIter<'a, RelationIdentity, Budgeted<SequenceRecord>>>,
}

impl<'a> Iterator for SequenceRecordIter<'a> {
    type Item = (&'a RelationIdentity, &'a SequenceRecord);

    fn next(&mut self) -> Option<Self::Item> {
        let entry = match (self.durable.peek(), self.temporary.peek()) {
            (Some(left), Some(right)) if left.0 < right.0 => self.durable.next(),
            (_, Some(_)) => self.temporary.next(),
            (Some(_), None) => self.durable.next(),
            (None, None) => None,
        };
        entry.map(|(key, value)| (key, &**value))
    }
}

pub struct CapturedSequenceRecords {
    root: RetainedMapSnapshot<RelationIdentity, Budgeted<SequenceRecord>>,
}

impl CapturedSequenceRecords {
    pub fn len(&self) -> usize {
        self.root.len()
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }
    pub fn get(&self, relation: &RelationIdentity) -> Option<&SequenceRecord> {
        self.root.get(relation).map(|value| &**value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&RelationIdentity, &SequenceRecord)> {
        self.root.iter().map(|(key, value)| (key, &**value))
    }
}
