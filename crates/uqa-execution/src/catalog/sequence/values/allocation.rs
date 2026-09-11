//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve sequence blocks, consume session caches and publish allocation observations.
use super::{NextvalTarget, SequenceValueContext, SequenceValueError};
use crate::catalog::sequence::{
    session::{
        NontransactionalSequenceValue, SessionLastSequenceReference, SessionSequenceCache,
        SessionSequenceValue,
    },
    SequenceState,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_storage::SequenceReservationResult;
impl SequenceValueContext<'_> {
    pub(super) fn take_cached_nextval(
        target: &NextvalTarget,
        caches: &mut BTreeMap<RelationIdentity, SessionSequenceCache>,
    ) -> Result<Option<(i64, bool)>, SequenceValueError> {
        let cache_relation = caches
            .contains_key(&target.relation)
            .then(|| target.relation.clone())
            .or_else(|| {
                caches.iter().find_map(|(relation, cache)| {
                    (cache.object_id == target.object_id).then(|| relation.clone())
                })
            });
        let Some(cache_relation) = cache_relation else {
            return Ok(None);
        };
        let cache = caches
            .remove(&cache_relation)
            .expect("selected sequence cache entry must exist");
        if cache.object_id != target.object_id
            || cache.definition_generation != target.state.definition_generation
        {
            return Ok(None);
        }
        let current = cache.next_value;
        if cache.remaining > 1 {
            let next_value = current.checked_add(target.state.increment).ok_or_else(|| {
                SequenceValueError::Internal(format!(
                    "cached sequence `{}` value overflow",
                    target.name
                ))
            })?;
            caches.insert(
                target.relation.clone(),
                SessionSequenceCache {
                    next_value,
                    remaining: cache.remaining - 1,
                    ..cache
                },
            );
        }
        Ok(Some((current, cache.autonomous)))
    }
    pub(super) fn reserve_nextval_block(
        &self,
        target: &NextvalTarget,
    ) -> Result<Option<(uqa_storage::SequenceValueReservation, bool)>, SequenceValueError> {
        let sequence_session = if target.temporary {
            None
        } else {
            self.runtime
                .open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        let autonomous = sequence_session.is_some();
        if !target.temporary && sequence_session.is_none() {
            self.runtime
                .prepare_explicit_transaction_writer()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                })?;
        }
        let catalog = (!target.temporary)
            .then(|| {
                sequence_session
                    .as_ref()
                    .map(|session| session.catalog.as_ref())
                    .or(self.storage)
            })
            .flatten();
        if let Some(catalog) = catalog {
            return match catalog.reserve_sequence_values(
                &target.name,
                target.object_id,
                target.state.definition_generation,
            ) {
                Ok(SequenceReservationResult::Reserved(reservation)) => {
                    Ok(Some((reservation, autonomous)))
                }
                Ok(SequenceReservationResult::DefinitionChanged) => Ok(None),
                Ok(SequenceReservationResult::Missing) => {
                    Err(SequenceValueError::Undefined(target.name.clone()))
                }
                Ok(SequenceReservationResult::Exhausted) => {
                    Err(exhausted(&target.name, target.state))
                }
                Err(error) => Err(SequenceValueError::Internal(format!(
                    "reserve sequence values: {error}"
                ))),
            };
        }
        let mut sequences = self.runtime.states_write();
        let sequence = sequences
            .get_mut(&target.relation)
            .ok_or_else(|| SequenceValueError::Undefined(target.name.clone()))?;
        if sequence.definition_generation != target.state.definition_generation {
            return Ok(None);
        }
        let reservation = uqa_storage::sequence_value_reservation(
            uqa_storage::SequenceValuePosition {
                current: sequence.current,
                called: sequence.called,
                log_count: sequence.log_count,
            },
            sequence.increment,
            sequence.min_value,
            sequence.max_value,
            sequence.cycle,
            sequence.cache_size,
        )
        .ok_or_else(|| exhausted(&target.name, *sequence))?;
        sequence.current = reservation.last_value;
        sequence.called = true;
        sequence.log_count = reservation.log_count;
        Ok(Some((reservation, autonomous)))
    }
    pub(super) fn install_nextval_reservation(
        &self,
        target: &NextvalTarget,
        reservation: uqa_storage::SequenceValueReservation,
        autonomous: bool,
        caches: &mut BTreeMap<RelationIdentity, SessionSequenceCache>,
    ) -> Result<SequenceState, SequenceValueError> {
        let mut physical = target.state;
        physical.current = reservation.last_value;
        physical.called = true;
        physical.log_count = reservation.log_count;
        if let Some(state) = self
            .runtime
            .states_write()
            .get_mut(&target.relation)
            .filter(|state| state.definition_generation == target.state.definition_generation)
        {
            state.current = reservation.last_value;
            state.called = true;
            state.log_count = reservation.log_count;
        }
        if reservation.count > 1 {
            let next_value = reservation
                .first_value
                .checked_add(target.state.increment)
                .ok_or_else(|| {
                    SequenceValueError::Internal(format!(
                        "cached sequence `{}` value overflow",
                        target.name
                    ))
                })?;
            caches.insert(
                target.relation.clone(),
                SessionSequenceCache {
                    object_id: target.object_id,
                    definition_generation: target.state.definition_generation,
                    next_value,
                    remaining: reservation.count - 1,
                    autonomous,
                },
            );
        }
        Ok(physical)
    }
    pub(super) fn complete_nextval(
        &self,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        current: i64,
        physical: SequenceState,
        autonomous: bool,
    ) {
        let mut session = self.runtime.session_write();
        session
            .currvals_mut()
            .retain(|_, value| value.object_id != object_id);
        session.currvals_mut().insert(
            relation.clone(),
            SessionSequenceValue {
                object_id,
                value: current,
            },
        );
        *session.last_mut() = Some(SessionLastSequenceReference {
            relation: relation.clone(),
            object_id,
        });
        drop(session);
        self.runtime.record_nontransactional_sequence_value(
            physical.definition_generation,
            NontransactionalSequenceValue {
                object_id,
                current: physical.current,
                called: physical.called,
                log_count: physical.log_count,
                autonomous,
            },
            true,
        );
    }
}
fn exhausted(name: &str, state: SequenceState) -> SequenceValueError {
    SequenceValueError::Exhausted {
        name: name.to_string(),
        bound: if state.increment > 0 {
            "maximum"
        } else {
            "minimum"
        },
        value: if state.increment > 0 {
            state.max_value
        } else {
            state.min_value
        },
    }
}

#[cfg(test)]
mod tests;
