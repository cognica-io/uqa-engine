//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime allocation and session state for SQL sequences.

use super::{
    BTreeMap, Engine, NontransactionalSequenceValue, RelationIdentity, SQLError,
    SequenceReservationResult, SequenceState, SessionSequenceCache, StorageBackendResult,
};

struct NextvalTarget {
    name: String,
    relation: RelationIdentity,
    object_id: [u8; 16],
    state: SequenceState,
    temporary: bool,
}

#[derive(Debug, thiserror::Error)]
enum SequenceValueError {
    #[error("relation \"{0}\" does not exist")]
    Undefined(String),
    #[error("cannot open relation \"{name}\": this operation is not supported for {kind}s")]
    WrongKind { name: String, kind: &'static str },
    #[error("currval of sequence \"{0}\" is not yet defined in this session")]
    CurrvalUndefined(String),
    #[error("lastval is not yet defined in this session")]
    LastvalUndefined,
    #[error("setval: value {value} is out of bounds for sequence \"{name}\" ({min}..{max})")]
    SetvalOutOfBounds {
        name: String,
        value: i64,
        min: i64,
        max: i64,
    },
    #[error("nextval: reached {bound} value of sequence \"{name}\" ({value})")]
    Exhausted {
        name: String,
        bound: &'static str,
        value: i64,
    },
    #[error("cannot execute {0}() in a read-only transaction")]
    ReadOnly(&'static str),
    #[error("{0}")]
    Internal(String),
}

impl SequenceValueError {
    fn exhausted(name: &str, state: SequenceState) -> Self {
        Self::Exhausted {
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

    fn into_sql_error(self) -> SQLError {
        let sqlstate = match self {
            Self::Undefined(_) => "42P01",
            Self::WrongKind { .. } => "42809",
            Self::CurrvalUndefined(_) | Self::LastvalUndefined => "55000",
            Self::SetvalOutOfBounds { .. } => "22003",
            Self::Exhausted { .. } => "2200H",
            Self::ReadOnly(_) => "25006",
            Self::Internal(message) => return SQLError::Internal(message),
        };
        SQLError::Routine {
            sqlstate: sqlstate.into(),
            message: self.to_string(),
        }
    }
}

impl Engine {
    fn record_nontransactional_sequence_value(
        &self,
        relation: &RelationIdentity,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        defines_lastval: bool,
    ) {
        let session_currval = self
            .session
            .state
            .read()
            .sequence_currvals
            .get(relation)
            .copied();
        let mut transactions = self.session.transactions.lock();
        for frame in transactions.iter_mut() {
            if defines_lastval {
                for history in frame.nontransactional_sequence_values.values_mut() {
                    history.defines_lastval = false;
                }
            }
            let history = frame
                .nontransactional_sequence_values
                .entry(relation.clone())
                .or_default();
            let preserves_lastval =
                !defines_lastval && history.object_id == value.object_id && history.defines_lastval;
            history
                .values_by_definition
                .insert(definition_generation, value);
            history.object_id = value.object_id;
            history.session_currval = session_currval;
            history.defines_lastval = defines_lastval || preserves_lastval;
        }
    }

    fn resolve_sequence_value_target(
        &self,
        reference: &str,
    ) -> Result<(String, RelationIdentity, [u8; 16]), SequenceValueError> {
        let (name, kind) = self
            .try_resolve_relation_kind(reference)
            .map_err(|error| {
                SequenceValueError::Internal(format!("load sequence catalog: {error}"))
            })?
            .ok_or_else(|| SequenceValueError::Undefined(reference.to_string()))?;
        if kind != "sequence" {
            return Err(SequenceValueError::WrongKind {
                name: reference.to_string(),
                kind,
            });
        }
        let relation = Self::resolved_relation_identity(&name).map_err(|error| {
            SequenceValueError::Internal(format!("resolve sequence `{name}`: {error}"))
        })?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SequenceValueError::Internal(format!(
                    "sequence `{name}` has no durable object identity"
                ))
            })?;
        Ok((name, relation, object_id))
    }

    pub fn nextval(&self, name: &str) -> Result<i64, String> {
        self.nextval_inner(name).map_err(|error| error.to_string())
    }

    pub(crate) fn nextval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.nextval_inner(name)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn nextval_inner(&self, name: &str) -> Result<i64, SequenceValueError> {
        loop {
            let target = self.resolve_nextval_target(name)?;
            let mut caches = self.session.sequence_caches.lock();
            if let Some((current, autonomous)) = Self::take_cached_nextval(&target, &mut caches)? {
                drop(caches);
                self.complete_nextval(
                    &target.relation,
                    target.object_id,
                    current,
                    target.state,
                    autonomous,
                );
                return Ok(current);
            }
            let Some((reservation, autonomous)) = self.reserve_nextval_block(&target)? else {
                drop(caches);
                continue;
            };
            let physical =
                self.install_nextval_reservation(&target, reservation, autonomous, &mut caches)?;
            drop(caches);
            self.complete_nextval(
                &target.relation,
                target.object_id,
                reservation.first_value,
                physical,
                autonomous,
            );
            return Ok(reservation.first_value);
        }
    }

    fn resolve_nextval_target(&self, name: &str) -> Result<NextvalTarget, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        let state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        if self.current_transaction_is_read_only() && !temporary {
            return Err(SequenceValueError::ReadOnly("nextval"));
        }
        Ok(NextvalTarget {
            name,
            relation,
            object_id,
            state,
            temporary,
        })
    }

    fn take_cached_nextval(
        target: &NextvalTarget,
        caches: &mut BTreeMap<RelationIdentity, SessionSequenceCache>,
    ) -> Result<Option<(i64, bool)>, SequenceValueError> {
        let Some(cache) = caches.remove(&target.relation) else {
            return Ok(None);
        };
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

    fn reserve_nextval_block(
        &self,
        target: &NextvalTarget,
    ) -> Result<Option<(uqa_storage::SequenceValueReservation, bool)>, SequenceValueError> {
        let sequence_session = if target.temporary {
            None
        } else {
            self.open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        let autonomous = sequence_session.is_some();
        if !target.temporary && sequence_session.is_none() {
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                })?;
        }
        let catalog = (!target.temporary)
            .then(|| {
                sequence_session
                    .as_ref()
                    .map(|session| session.catalog.as_ref())
                    .or(self.storage.catalog.as_deref())
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
                    Err(SequenceValueError::exhausted(&target.name, target.state))
                }
                Err(error) => Err(SequenceValueError::Internal(format!(
                    "reserve sequence values: {error}"
                ))),
            };
        }
        let mut sequences = self.durable.sequences.write();
        let sequence = sequences
            .get_mut(&target.relation)
            .ok_or_else(|| SequenceValueError::Undefined(target.name.clone()))?;
        if sequence.definition_generation != target.state.definition_generation {
            return Ok(None);
        }
        let reservation = uqa_storage::sequence_value_reservation(
            sequence.current,
            sequence.called,
            sequence.increment,
            sequence.min_value,
            sequence.max_value,
            sequence.cycle,
            sequence.cache_size,
        )
        .ok_or_else(|| SequenceValueError::exhausted(&target.name, *sequence))?;
        sequence.current = reservation.last_value;
        sequence.called = true;
        Ok(Some((reservation, autonomous)))
    }

    fn install_nextval_reservation(
        &self,
        target: &NextvalTarget,
        reservation: uqa_storage::SequenceValueReservation,
        autonomous: bool,
        caches: &mut BTreeMap<RelationIdentity, SessionSequenceCache>,
    ) -> Result<SequenceState, SequenceValueError> {
        let mut physical = target.state;
        physical.current = reservation.last_value;
        physical.called = true;
        if let Some(state) = self
            .durable
            .sequences
            .write()
            .get_mut(&target.relation)
            .filter(|state| state.definition_generation == target.state.definition_generation)
        {
            state.current = reservation.last_value;
            state.called = true;
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

    fn complete_nextval(
        &self,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        current: i64,
        physical: SequenceState,
        autonomous: bool,
    ) {
        let mut session = self.session.state.write();
        session.sequence_currvals.insert(
            relation.clone(),
            super::SessionSequenceValue {
                object_id,
                value: current,
            },
        );
        session.last_sequence = Some(super::SessionLastSequenceReference {
            relation: relation.clone(),
            object_id,
        });
        drop(session);
        self.record_nontransactional_sequence_value(
            relation,
            physical.definition_generation,
            NontransactionalSequenceValue {
                object_id,
                current: physical.current,
                called: physical.called,
                autonomous,
            },
            true,
        );
    }

    pub fn currval(&self, name: &str) -> Result<i64, String> {
        self.currval_inner(name).map_err(|error| error.to_string())
    }

    pub(crate) fn currval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.currval_inner(name)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn currval_inner(&self, name: &str) -> Result<i64, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        self.session
            .state
            .read()
            .sequence_currvals
            .get(&relation)
            .copied()
            .filter(|current| current.object_id == object_id)
            .map(|current| current.value)
            .ok_or(SequenceValueError::CurrvalUndefined(name))
    }

    pub fn lastval(&self) -> Result<i64, String> {
        self.lastval_inner().map_err(|error| error.to_string())
    }

    pub(crate) fn lastval_sql(&self) -> Result<i64, SQLError> {
        self.lastval_inner()
            .map_err(SequenceValueError::into_sql_error)
    }

    fn lastval_inner(&self) -> Result<i64, SequenceValueError> {
        self.refresh_sequences_from_catalog().map_err(|error| {
            SequenceValueError::Internal(format!("load sequence catalog: {error}"))
        })?;
        let object_ids = self.durable.sequence_object_ids.read();
        let session = self.session.state.read();
        let last = session
            .last_sequence
            .as_ref()
            .ok_or(SequenceValueError::LastvalUndefined)?;
        if object_ids.get(&last.relation).copied() != Some(last.object_id) {
            return Err(SequenceValueError::LastvalUndefined);
        }
        session
            .sequence_currvals
            .get(&last.relation)
            .copied()
            .filter(|current| current.object_id == last.object_id)
            .map(|current| current.value)
            .ok_or(SequenceValueError::LastvalUndefined)
    }

    pub fn setval(&self, name: &str, value: i64) -> Result<i64, String> {
        self.setval_inner(name, value, true)
            .map_err(|error| error.to_string())
    }

    pub fn setval_with_is_called(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, String> {
        self.setval_inner(name, value, is_called)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn setval_sql(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SQLError> {
        self.setval_inner(name, value, is_called)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn setval_inner(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        let previous = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        if self.current_transaction_is_read_only() && !temporary {
            return Err(SequenceValueError::ReadOnly("setval"));
        }
        let (min, max) = (previous.min_value, previous.max_value);
        if !(min..=max).contains(&value) {
            return Err(SequenceValueError::SetvalOutOfBounds {
                name,
                value,
                min,
                max,
            });
        }
        let sequence_session = if temporary {
            None
        } else {
            self.open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        let autonomous = sequence_session.is_some();
        if !temporary && sequence_session.is_none() {
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                })?;
        }
        let catalog = if temporary {
            None
        } else {
            sequence_session
                .as_ref()
                .map(|session| session.catalog.as_ref())
                .or(self.storage.catalog.as_deref())
        };
        if let Some(catalog) = catalog {
            catalog
                .set_sequence_value(&name, object_id, value, is_called)
                .map_err(|error| {
                    SequenceValueError::Internal(format!("persist sequence value: {error}"))
                })?
                .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        }
        let mut seqs = self.durable.sequences.write();
        let seq = seqs
            .get_mut(&relation)
            .ok_or(SequenceValueError::Undefined(name))?;
        seq.current = value;
        seq.called = is_called;
        drop(seqs);
        self.session.sequence_caches.lock().remove(&relation);
        if is_called {
            self.session.state.write().sequence_currvals.insert(
                relation.clone(),
                super::SessionSequenceValue { object_id, value },
            );
        }
        self.record_nontransactional_sequence_value(
            &relation,
            previous.definition_generation,
            NontransactionalSequenceValue {
                object_id,
                current: value,
                called: is_called,
                autonomous,
            },
            false,
        );
        Ok(value)
    }

    pub(super) fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageSession>> {
        if !self.backend_transaction_is_deferred()
            || self.session.row_lock_statements.lock().is_empty()
        {
            return Ok(None);
        }
        self.storage
            .provider
            .as_ref()
            .map_or(Ok(None), |provider| provider.open_session().map(Some))
    }
}
