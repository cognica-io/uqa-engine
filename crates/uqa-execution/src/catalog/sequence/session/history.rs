//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable value history admits position and session successors before allocation effects.

use super::{NontransactionalSequenceValue, SessionSequenceValue};
use uqa_core::memory::{
    MemoryBudget, MemoryError, PreparedRetainedMapInsert, RetainedMap, RetainedMapSnapshot,
};

pub type SequenceValueKey = ([u8; 16], [u8; 16]);

/// Original durable definitions and other rollback history have disjoint membership. Only the former participates in the captured image allowance; both remain available to transaction recovery.
#[derive(Clone)]
pub struct NontransactionalSequenceValues {
    original: RetainedMap<SequenceValueKey, NontransactionalSequenceValue>,
    other: RetainedMap<SequenceValueKey, NontransactionalSequenceValue>,
    observations: RetainedMap<[u8; 16], Option<SessionSequenceValue>>,
    lastval: Option<[u8; 16]>,
}

impl Default for NontransactionalSequenceValues {
    fn default() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }
}

impl NontransactionalSequenceValues {
    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            original: RetainedMap::new(memory),
            other: RetainedMap::new(memory),
            observations: RetainedMap::new(memory),
            lastval: None,
        }
    }
    pub fn position(
        &self,
        object: [u8; 16],
        definition: [u8; 16],
    ) -> Option<&NontransactionalSequenceValue> {
        self.original
            .get(&(object, definition))
            .or_else(|| self.other.get(&(object, definition)))
    }
    pub fn observations(&self) -> impl Iterator<Item = (&[u8; 16], &Option<SessionSequenceValue>)> {
        self.observations.iter()
    }
    pub fn lastval(&self) -> Option<[u8; 16]> {
        self.lastval
    }
    pub fn is_empty(&self) -> bool {
        self.original.is_empty() && self.other.is_empty()
    }
    pub fn prepare(
        &mut self,
        key: SequenceValueKey,
        original: bool,
    ) -> Result<PreparedSequenceHistory<'_>, MemoryError> {
        let values = if original {
            &mut self.original
        } else {
            &mut self.other
        };
        Ok(PreparedSequenceHistory {
            value: values.prepare_insert(key, 0)?,
            observation: self.observations.prepare_insert(key.0, 0)?,
            object: key.0,
            lastval: &mut self.lastval,
        })
    }
    pub fn discard_session_values(&mut self) {
        self.observations = RetainedMap::new(self.observations.budget());
        self.lastval = None;
    }
    pub(crate) fn capture_original(
        &self,
        memory: &MemoryBudget,
    ) -> Result<
        uqa_core::memory::Budgeted<
            RetainedMapSnapshot<SequenceValueKey, NontransactionalSequenceValue>,
        >,
        MemoryError,
    > {
        self.original.capture(memory)
    }
}

pub struct PreparedSequenceHistory<'a> {
    value: PreparedRetainedMapInsert<'a, SequenceValueKey, NontransactionalSequenceValue>,
    observation: PreparedRetainedMapInsert<'a, [u8; 16], Option<SessionSequenceValue>>,
    object: [u8; 16],
    lastval: &'a mut Option<[u8; 16]>,
}

impl PreparedSequenceHistory<'_> {
    pub fn publish(
        self,
        value: NontransactionalSequenceValue,
        session_currval: Option<SessionSequenceValue>,
        defines_lastval: bool,
    ) {
        assert_eq!(
            self.object, value.object_id,
            "prepared sequence value identity"
        );
        self.value.publish(value);
        self.observation.publish(session_currval);
        if defines_lastval {
            *self.lastval = Some(self.object);
        }
    }
}
