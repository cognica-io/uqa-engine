//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute SQL sequence value functions against retained runtime and provider inputs.
mod allocation;
pub mod context;
mod resolution;
use super::{
    session::{NontransactionalSequenceValue, SessionSequenceValue},
    SequenceState,
};
use context::SequenceValueContext;
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
        loop {
            let target = self.resolve_nextval_target(name)?;
            let mut caches = self.runtime.caches();
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
    pub fn currval(&self, name: &str) -> Result<i64, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        self.privileges
            .ensure_sequence_currval_privilege(&name, &relation)?;
        self.runtime
            .session_read()
            .currvals()
            .values()
            .find(|current| current.object_id == object_id)
            .map(|current| current.value)
            .ok_or(SequenceValueError::CurrvalUndefined(relation.name))
    }
    pub fn lastval(&self) -> Result<i64, SequenceValueError> {
        self.sequences.refresh_sequences().map_err(|error| {
            SequenceValueError::Internal(format!("load sequence catalog: {error}"))
        })?;
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
        let relation = self
            .sequences
            .object_ids()
            .iter()
            .find_map(|(relation, candidate)| (*candidate == object_id).then(|| relation.clone()))
            .ok_or(SequenceValueError::LastvalUndefined)?;
        let name = relation.qualified_name();
        self.privileges
            .ensure_sequence_currval_privilege(&name, &relation)?;
        Ok(value)
    }
    pub fn setval(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SequenceValueError> {
        let (name, relation, object_id) = self.resolve_sequence_value_target(name)?;
        let previous = self
            .sequences
            .states()
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = self
            .runtime
            .persistence()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        self.privileges
            .ensure_sequence_setval_privilege(&name, &relation)?;
        if self.runtime.current_transaction_is_read_only() && !temporary {
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
        self.setval_target(
            NextvalTarget {
                name,
                relation,
                object_id,
                state: previous,
                temporary,
            },
            value,
            is_called,
        )
    }
    fn setval_target(
        &self,
        target: NextvalTarget,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SequenceValueError> {
        let NextvalTarget {
            name,
            relation,
            object_id,
            state: previous,
            temporary,
        } = target;
        let sequence_session = if temporary {
            None
        } else {
            self.runtime
                .open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        let autonomous = sequence_session.is_some();
        if !temporary && sequence_session.is_none() {
            self.runtime
                .prepare_explicit_transaction_writer()
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
                .or(self.storage)
        };
        if let Some(catalog) = catalog {
            catalog
                .set_sequence_value(&name, object_id, value, is_called, 0)
                .map_err(|error| {
                    SequenceValueError::Internal(format!("persist sequence value: {error}"))
                })?
                .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        }
        let mut seqs = self.runtime.states_write();
        let seq = seqs
            .get_mut(&relation)
            .ok_or(SequenceValueError::Undefined(name))?;
        seq.current = value;
        seq.called = is_called;
        seq.log_count = 0;
        drop(seqs);
        self.runtime
            .caches()
            .retain(|_, cache| cache.object_id != object_id);
        if is_called {
            let mut session = self.runtime.session_write();
            session
                .currvals_mut()
                .retain(|_, current| current.object_id != object_id);
            session
                .currvals_mut()
                .insert(relation.clone(), SessionSequenceValue { object_id, value });
        }
        self.runtime.record_nontransactional_sequence_value(
            previous.definition_generation,
            NontransactionalSequenceValue {
                object_id,
                current: value,
                called: is_called,
                log_count: 0,
                autonomous,
            },
            false,
        );
        Ok(value)
    }
}
