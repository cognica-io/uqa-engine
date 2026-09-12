//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Engine, NontransactionalSequenceValue};
use std::cell::RefCell;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::sequence::{
    restoration::SequencePersistenceRead,
    values::context::{
        SequenceCachesWrite, SequenceSessionRead, SequenceSessionWrite, SequenceStatesWrite,
        SequenceValueRuntime,
    },
};
use uqa_sql::{catalog::sequence_functions::value_error::SequenceValueError, SQLError};
use uqa_storage::{PersistentStorageSession, StorageBackendResult};

struct RuntimeObserver<'a> {
    engine: &'a Engine,
    allocating: bool,
    fail_writer: bool,
    events: RefCell<Vec<&'static str>>,
}
impl SequenceValueRuntime for RuntimeObserver<'_> {
    fn persistence(&self) -> SequencePersistenceRead<'_> {
        SequenceValueRuntime::persistence(self.engine)
    }
    fn states_write(&self) -> SequenceStatesWrite<'_> {
        SequenceValueRuntime::states_write(self.engine)
    }
    fn caches(&self) -> SequenceCachesWrite<'_> {
        SequenceValueRuntime::caches(self.engine)
    }
    fn session_read(&self) -> Box<dyn SequenceSessionRead + '_> {
        SequenceValueRuntime::session_read(self.engine)
    }
    fn session_write(&self) -> Box<dyn SequenceSessionWrite + '_> {
        assert!(!self.engine.session.sequence_caches.is_locked());
        SequenceValueRuntime::session_write(self.engine)
    }
    fn current_transaction_is_read_only(&self) -> bool {
        SequenceValueRuntime::current_transaction_is_read_only(self.engine)
    }
    fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<PersistentStorageSession>> {
        assert_eq!(
            self.engine.session.sequence_caches.is_locked(),
            self.allocating
        );
        assert!(!self.engine.durable.sequences.is_locked());
        self.events.borrow_mut().push("open");
        SequenceValueRuntime::open_nontransactional_sequence_session(self.engine)
    }
    fn prepare_explicit_transaction_writer(&self) -> Result<(), SQLError> {
        assert_eq!(
            self.engine.session.sequence_caches.is_locked(),
            self.allocating
        );
        self.events.borrow_mut().push("writer");
        if self.fail_writer {
            return Err(SQLError::Internal(
                "injected sequence writer failure".into(),
            ));
        }
        SequenceValueRuntime::prepare_explicit_transaction_writer(self.engine)
    }
    fn record_nontransactional_sequence_value(
        &self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        defines_lastval: bool,
    ) {
        assert!(!self.engine.session.sequence_caches.is_locked());
        assert!(!self.engine.session.state.is_locked());
        assert!(!self.engine.durable.sequences.is_locked());
        self.events.borrow_mut().push("record");
        SequenceValueRuntime::record_nontransactional_sequence_value(
            self.engine,
            definition_generation,
            value,
            defines_lastval,
        );
    }
}
fn observer(engine: &Engine, allocating: bool, fail_writer: bool) -> RuntimeObserver<'_> {
    RuntimeObserver {
        engine,
        allocating,
        fail_writer,
        events: RefCell::new(Vec::new()),
    }
}
#[test]
fn nextval_retains_the_actual_cache_guard_through_reservation_and_releases_it_before_history() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE ids CACHE 3", &[]).unwrap();
    let runtime = observer(&engine, true, false);
    let mut context = engine.sequence_value_context();
    context.runtime = &runtime;
    assert_eq!(context.nextval("ids").unwrap(), 1);
    assert_eq!(*runtime.events.borrow(), ["open", "writer", "record"]);
    assert_eq!(context.nextval("ids").unwrap(), 2);
    assert_eq!(
        *runtime.events.borrow(),
        ["open", "writer", "record", "record"]
    );
    assert_eq!(context.currval("ids").unwrap(), 2);
    assert_eq!(context.lastval().unwrap(), 2);
    assert_eq!(engine.sequence_state("ids").unwrap().unwrap().1.current, 3);
}
#[test]
fn failed_sequence_writer_leaves_real_allocation_cache_and_session_values_unchanged() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE ids CACHE 3; CREATE SEQUENCE other CACHE 4; SELECT nextval('other')",
            &[],
        )
        .unwrap();
    let states = engine.durable.sequences.read().clone();
    let caches = engine.session.sequence_caches.lock().clone();
    let values = engine.session.state.read().sequence_currvals.clone();
    let last = engine.session.state.read().last_sequence.clone();
    for allocating in [true, false] {
        let runtime = observer(&engine, allocating, true);
        let mut context = engine.sequence_value_context();
        context.runtime = &runtime;
        let result = if allocating {
            context.nextval("ids")
        } else {
            context.setval("ids", 30, true)
        };
        let error = result.unwrap_err();
        assert!(
            matches!(error, SequenceValueError::Internal(message) if message.contains("prepare sequence writer:") && message.contains("injected sequence writer failure"))
        );
        assert_eq!(*runtime.events.borrow(), ["open", "writer"]);
        assert_eq!(*engine.durable.sequences.read(), states);
        assert!(*engine.session.sequence_caches.lock() == caches);
        assert!(engine.session.state.read().sequence_currvals == values);
        assert!(engine.session.state.read().last_sequence == last);
    }
}
#[test]
fn setval_without_is_called_preserves_session_history_and_unrelated_cache_across_rollback() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE ids CACHE 3; CREATE SEQUENCE other START 101 CACHE 4; SELECT nextval('ids'); SELECT nextval('other')", &[]).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    let values = engine.session.state.read().sequence_currvals.clone();
    let last = engine.session.state.read().last_sequence.clone();
    let other = RelationIdentity::new("public", "other");
    let other_cache = engine.session.sequence_caches.lock()[&other];
    let runtime = observer(&engine, false, false);
    let mut context = engine.sequence_value_context();
    context.runtime = &runtime;
    assert_eq!(context.setval("ids", 42, false).unwrap(), 42);
    assert_eq!(*runtime.events.borrow(), ["open", "writer", "record"]);
    assert!(engine.session.state.read().sequence_currvals == values);
    assert!(engine.session.state.read().last_sequence == last);
    assert_eq!(engine.session.sequence_caches.lock().len(), 1);
    assert!(engine.session.sequence_caches.lock()[&other] == other_cache);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.currval("ids").unwrap(), 1);
    assert_eq!(engine.lastval().unwrap(), 101);
    assert_eq!(engine.nextval("ids").unwrap(), 42);
    assert_eq!(engine.nextval("other").unwrap(), 102);
}
