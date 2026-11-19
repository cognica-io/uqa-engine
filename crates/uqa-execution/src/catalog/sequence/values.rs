//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute SQL sequence value functions against retained runtime and provider inputs.
mod allocation;
pub mod context;
mod persistence;
mod positions;
mod resolution;
use super::{
    session::{NontransactionalSequenceValue, SessionSequenceValue},
    SequenceState,
};
use context::SequenceValueContext;
use resolution::{BoundSequenceValue, ValueAccess};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::sequence_functions::value_error::SequenceValueError;
struct NextvalTarget {
    name: String,
    relation: RelationIdentity,
    object_id: [u8; 16],
    state: SequenceState,
    temporary: bool,
}

impl SequenceValueContext<'_> {
    pub fn nextval(&self, name: &str) -> Result<i64, SequenceValueError> {
        self.transactions
            .with_value_transaction(Box::new(|| self.nextval_locked(name)))
    }

    fn nextval_locked(&self, name: &str) -> Result<i64, SequenceValueError> {
        let bound = self.bind_sequence_value_reference(name)?;
        loop {
            self.runtime.cancellation().check()?;
            let target = self.lock_sequence_value_target(&bound, ValueAccess::Next)?;
            if self.runtime.current_transaction_is_read_only() && !target.temporary {
                return Err(SequenceValueError::ReadOnly("nextval"));
            }
            if !target.temporary {
                self.runtime
                    .require_sequence_value_durability()
                    .map_err(|error| {
                        persistence::sequence_storage_error("retain sequence durability", error)
                    })?;
            }
            let mut caches = self.runtime.caches();
            if Self::has_cached_nextval(&target, &caches) {
                let result = self.runtime.with_sequence_histories(Box::new(|histories| {
                    let prepared = prepare_histories(histories, &target)?;
                    let (current, autonomous) = Self::take_cached_nextval(&target, &mut caches)?
                        .expect("selected sequence cache remains locked");
                    drop(caches);
                    self.complete_nextval(&target.relation, target.object_id, current);
                    publish_histories(
                        prepared,
                        &target,
                        target.state,
                        autonomous,
                        Some(SessionSequenceValue {
                            object_id: target.object_id,
                            value: current,
                        }),
                        true,
                    );
                    Ok(Some(current))
                }))?;
                return Ok(result.expect("cached sequence operation returns a value"));
            }
            let storage = self.value_storage(&target)?;
            let result = self.runtime.with_sequence_histories(Box::new(|histories| {
                let prepared = prepare_histories(histories, &target)?;
                let mut catalog = self.runtime.states_write();
                let existed = catalog.contains_key(&target.relation);
                let position = catalog.prepare_position(
                    &target.relation,
                    target.object_id,
                    target.state.definition_generation,
                )?;
                if position.is_none() && !storage.persistent() {
                    return if existed {
                        Ok(None)
                    } else {
                        Err(SequenceValueError::Undefined(target.name.clone()))
                    };
                }
                let state = position.as_ref().map_or(
                    target.state,
                    super::registry::PreparedSequencePosition::state,
                );
                let Some(block) = self.reserve_nextval_block(&target, &storage, state)? else {
                    return Ok(None);
                };
                let physical = Self::install_nextval_reservation(&target, block, &mut caches)?;
                if let Some(position) = position {
                    if !block.positioned {
                        position.publish(uqa_storage::SequenceValuePosition {
                            current: physical.current,
                            called: physical.called,
                            log_count: physical.log_count,
                        });
                    }
                }
                drop(catalog);
                drop(caches);
                self.complete_nextval(
                    &target.relation,
                    target.object_id,
                    block.reservation.first_value,
                );
                publish_histories(
                    prepared,
                    &target,
                    physical,
                    block.autonomous,
                    Some(SessionSequenceValue {
                        object_id: target.object_id,
                        value: block.reservation.first_value,
                    }),
                    true,
                );
                Ok(Some(block.reservation.first_value))
            }))?;
            if let Some(value) = result {
                return Ok(value);
            }
        }
    }
    pub fn currval(&self, name: &str) -> Result<i64, SequenceValueError> {
        self.transactions
            .with_value_transaction(Box::new(|| self.currval_locked(name)))
    }

    fn currval_locked(&self, name: &str) -> Result<i64, SequenceValueError> {
        let bound = self.bind_sequence_value_reference(name)?;
        let target = self.lock_sequence_value_target(&bound, ValueAccess::Current)?;
        self.runtime
            .session_read()
            .currvals()
            .values()
            .find(|current| current.object_id == target.object_id)
            .map(|current| current.value)
            .ok_or(SequenceValueError::CurrvalUndefined(target.relation.name))
    }
    pub fn lastval(&self) -> Result<i64, SequenceValueError> {
        self.transactions
            .with_value_transaction(Box::new(|| self.lastval_locked()))
    }

    fn lastval_locked(&self) -> Result<i64, SequenceValueError> {
        let snapshot = self.read_snapshot()?;
        let session = self.runtime.session_read();
        let last = session.last().ok_or(SequenceValueError::LastvalUndefined)?;
        let object_id = last.object_id;
        let value = session
            .currvals()
            .values()
            .find(|current| current.object_id == object_id)
            .map(|current| current.value)
            .ok_or(SequenceValueError::LastvalUndefined)?;
        drop(session);
        let relation = snapshot
            .catalog
            .object_ids()
            .find_map(|(relation, candidate)| (*candidate == object_id).then(|| relation.clone()))
            .ok_or(SequenceValueError::LastvalUndefined)?;
        self.lock_sequence_value_target(
            &BoundSequenceValue {
                name: relation.qualified_name(),
                object_id,
            },
            ValueAccess::Current,
        )?;
        Ok(value)
    }
    pub fn setval(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SequenceValueError> {
        self.transactions
            .with_value_transaction(Box::new(|| self.setval_locked(name, value, is_called)))
    }

    fn setval_locked(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SequenceValueError> {
        let bound = self.bind_sequence_value_reference(name)?;
        loop {
            self.runtime.cancellation().check()?;
            let target = self.lock_sequence_value_target(&bound, ValueAccess::Set)?;
            if self.runtime.current_transaction_is_read_only() && !target.temporary {
                return Err(SequenceValueError::ReadOnly("setval"));
            }
            let (min, max) = (target.state.min_value, target.state.max_value);
            if !(min..=max).contains(&value) {
                return Err(SequenceValueError::SetvalOutOfBounds {
                    name: target.relation.name,
                    value,
                    min,
                    max,
                });
            }
            if !target.temporary {
                self.runtime
                    .require_sequence_value_durability()
                    .map_err(|error| {
                        persistence::sequence_storage_error("retain sequence durability", error)
                    })?;
            }
            if let Some(value) = self.setval_target(target, value, is_called)? {
                return Ok(value);
            }
        }
    }
    fn setval_target(
        &self,
        target: NextvalTarget,
        value: i64,
        is_called: bool,
    ) -> Result<Option<i64>, SequenceValueError> {
        let storage = self.value_storage(&target)?;
        let mut caches = self.runtime.caches();
        self.runtime.with_sequence_histories(Box::new(|histories| {
            let prepared = prepare_histories(histories, &target)?;
            let mut catalog = self.runtime.states_write();
            let existed = catalog.contains_key(&target.relation);
            let native = catalog.prepare_position(
                &target.relation,
                target.object_id,
                target.state.definition_generation,
            )?;
            if native.is_none() && !storage.persistent() {
                return if existed {
                    Ok(None)
                } else {
                    Err(SequenceValueError::Undefined(target.name.clone()))
                };
            }
            // Hold the shared position until its replacement record is durable, so another allocator cannot continue the old log.
            let mut position = self.discard_position(&target)?;
            let persisted = storage.mutate(
                self.runtime.cancellation(),
                "persist sequence value",
                |storage| {
                    storage.set_sequence_value(
                        &target.name,
                        target.object_id,
                        target.state.definition_generation,
                        value,
                        is_called,
                        0,
                    )
                },
            )?;
            let autonomous = match persisted {
                Some((uqa_storage::SequenceSetValueResult::Set(_), autonomous)) => {
                    if let Some(position) = position.as_mut() {
                        position.discard()?;
                    }
                    autonomous
                }
                Some((uqa_storage::SequenceSetValueResult::DefinitionChanged, _)) => {
                    return Ok(None)
                }
                Some((uqa_storage::SequenceSetValueResult::Missing, _)) => {
                    return Err(SequenceValueError::Undefined(target.name.clone()))
                }
                None => false,
            };
            drop(position);
            let mut physical = target.state;
            physical.current = value;
            physical.called = is_called;
            physical.log_count = 0;
            if let Some(native) = native {
                native.publish(uqa_storage::SequenceValuePosition {
                    current: value,
                    called: is_called,
                    log_count: 0,
                });
            }
            drop(catalog);
            caches.retain(|_, cache| cache.object_id != target.object_id);
            drop(caches);
            if is_called {
                let mut session = self.runtime.session_write();
                session
                    .currvals_mut()
                    .retain(|_, current| current.object_id != target.object_id);
                session.currvals_mut().insert(
                    target.relation.clone(),
                    SessionSequenceValue {
                        object_id: target.object_id,
                        value,
                    },
                );
            }
            let currval = self
                .runtime
                .session_read()
                .currvals()
                .values()
                .find(|current| current.object_id == target.object_id)
                .copied();
            publish_histories(prepared, &target, physical, autonomous, currval, false);
            Ok(Some(value))
        }))
    }
}

fn prepare_histories<'a>(
    histories: &'a mut [context::SequenceHistoryWrite<'_>],
    target: &NextvalTarget,
) -> Result<Vec<super::session::PreparedSequenceHistory<'a>>, SequenceValueError> {
    histories
        .iter_mut()
        .map(|history| {
            history
                .prepare(target.object_id, target.state.definition_generation)
                .map_err(SequenceValueError::from)
        })
        .collect()
}

fn publish_histories(
    prepared: Vec<super::session::PreparedSequenceHistory<'_>>,
    target: &NextvalTarget,
    physical: SequenceState,
    autonomous: bool,
    currval: Option<SessionSequenceValue>,
    defines_lastval: bool,
) {
    for history in prepared {
        history.publish(
            NontransactionalSequenceValue {
                object_id: target.object_id,
                current: physical.current,
                called: physical.called,
                log_count: physical.log_count,
                autonomous,
            },
            currval,
            defines_lastval,
        );
    }
}
