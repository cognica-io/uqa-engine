//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind native sequence values to actual Engine session and catalog guards.
use crate::{
    Engine, NontransactionalSequenceValue, SessionLastSequenceReference, SessionSequenceValue,
    SessionStateSnapshot,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::sequence::{
    restoration::SequencePersistenceRead,
    values::context::{
        SequenceCachesWrite, SequenceSessionRead, SequenceSessionWrite, SequenceStatesWrite,
        SequenceValueContext, SequenceValueRuntime,
    },
};
use uqa_sql::{catalog::sequence_functions::value_error::SequenceValueError, SQLError};
use uqa_storage::{PersistentStorageSession, StorageBackendResult};
struct SessionRead<'a>(parking_lot::RwLockReadGuard<'a, SessionStateSnapshot>);
struct SessionWrite<'a>(parking_lot::RwLockWriteGuard<'a, SessionStateSnapshot>);
impl SequenceSessionRead for SessionRead<'_> {
    fn currvals(&self) -> &BTreeMap<RelationIdentity, SessionSequenceValue> {
        &self.0.sequence_currvals
    }
    fn last(&self) -> Option<&SessionLastSequenceReference> {
        self.0.last_sequence.as_ref()
    }
}
impl SequenceSessionWrite for SessionWrite<'_> {
    fn currvals_mut(&mut self) -> &mut BTreeMap<RelationIdentity, SessionSequenceValue> {
        &mut self.0.sequence_currvals
    }
    fn last_mut(&mut self) -> &mut Option<SessionLastSequenceReference> {
        &mut self.0.last_sequence
    }
}
impl SequenceValueRuntime for Engine {
    fn persistence(&self) -> SequencePersistenceRead<'_> {
        Box::new(self.durable.sequence_persistence.read())
    }
    fn states_write(&self) -> SequenceStatesWrite<'_> {
        Box::new(self.durable.sequences.write())
    }
    fn caches(&self) -> SequenceCachesWrite<'_> {
        Box::new(self.session.sequence_caches.lock())
    }
    fn session_read(&self) -> Box<dyn SequenceSessionRead + '_> {
        Box::new(SessionRead(self.session.state.read()))
    }
    fn session_write(&self) -> Box<dyn SequenceSessionWrite + '_> {
        Box::new(SessionWrite(self.session.state.write()))
    }
    fn current_transaction_is_read_only(&self) -> bool {
        Engine::current_transaction_is_read_only(self)
    }
    fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<PersistentStorageSession>> {
        Engine::open_nontransactional_sequence_session(self)
    }
    fn prepare_explicit_transaction_writer(&self) -> Result<(), SQLError> {
        Engine::prepare_explicit_transaction_writer(self).map(|_| ())
    }
    fn record_nontransactional_sequence_value(
        &self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        defines_lastval: bool,
    ) {
        Engine::record_nontransactional_sequence_value(
            self,
            definition_generation,
            value,
            defines_lastval,
        );
    }
}
impl Engine {
    pub(crate) fn sequence_value_context(&self) -> SequenceValueContext<'_> {
        SequenceValueContext {
            sequences: self,
            privileges: self.sequence_privilege_inquiry(),
            runtime: self,
            storage: self.storage.catalog.as_deref(),
        }
    }
    pub fn nextval(&self, name: &str) -> Result<i64, String> {
        self.sequence_value_context()
            .nextval(name)
            .map_err(|error| error.to_string())
    }
    pub(crate) fn nextval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.sequence_value_context()
            .nextval(name)
            .map_err(SequenceValueError::into_sql_error)
    }
    pub fn currval(&self, name: &str) -> Result<i64, String> {
        self.sequence_value_context()
            .currval(name)
            .map_err(|error| error.to_string())
    }
    pub(crate) fn currval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.sequence_value_context()
            .currval(name)
            .map_err(SequenceValueError::into_sql_error)
    }
    pub fn lastval(&self) -> Result<i64, String> {
        self.sequence_value_context()
            .lastval()
            .map_err(|error| error.to_string())
    }
    pub(crate) fn lastval_sql(&self) -> Result<i64, SQLError> {
        self.sequence_value_context()
            .lastval()
            .map_err(SequenceValueError::into_sql_error)
    }
    pub fn setval(&self, name: &str, value: i64) -> Result<i64, String> {
        self.sequence_value_context()
            .setval(name, value, true)
            .map_err(|error| error.to_string())
    }
    pub fn setval_with_is_called(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, String> {
        self.sequence_value_context()
            .setval(name, value, is_called)
            .map_err(|error| error.to_string())
    }
    pub(crate) fn setval_sql(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> Result<i64, SQLError> {
        self.sequence_value_context()
            .setval(name, value, is_called)
            .map_err(SequenceValueError::into_sql_error)
    }
}
