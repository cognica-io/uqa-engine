//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical session histories and lost completion replies use a separate fault-injectable SSI owner.

use super::*;

#[path = "serializable/admission.rs"]
mod admission;

#[path = "serializable/batches.rs"]
mod batches;

#[path = "serializable/graph_topology.rs"]
mod graph_topology;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckpointFault {
    None,
    Reject,
    LoseReply,
}

pub(super) struct Fixture {
    checkpoint: Vec<u8>,
    leases: Arc<LocalSerializableLeases>,
    admissions: usize,
    fault: (usize, CheckpointFault),
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self {
            checkpoint: Vec::new(),
            leases: Arc::new(LocalSerializableLeases::new(&MemoryBudget::new(1 << 24))),
            admissions: 0,
            fault: (0, CheckpointFault::None),
        }
    }
}

struct Leases(Arc<LocalSerializableLeases>);
impl SerializableLeases for Leases {
    fn retain(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableParticipant> {
        self.0.retain(id, control)
    }
    fn is_alive(
        &self,
        id: SerializableTransactionId,
        _: &StorageReadControl,
    ) -> VersionResult<bool> {
        Ok(self.0.is_alive(id))
    }
    fn reclaim(&self) {
        self.0.reclaim();
    }
}

impl SerializableCoordinator for Persistence {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let mut fixture = self.serializable.lock();
        fixture.admissions += 1;
        let fault = if fixture.admissions == fixture.fault.0 {
            fixture.fault.1
        } else {
            CheckpointFault::None
        };
        let mut graph = if fixture.checkpoint.is_empty() {
            SerializableGraph::new(self.database_id(), [8; 16], control.memory())?
        } else {
            SerializableGraph::read_checkpoint(
                self.database_id(),
                [8; 16],
                &mut fixture.checkpoint.as_slice(),
                control,
            )?
        };
        let result = operation(&mut graph, &Leases(Arc::clone(&fixture.leases)));
        if fault == CheckpointFault::Reject {
            return Err(StorageBackendError::Other("injected checkpoint rejection".into()).into());
        }
        let mut checkpoint = Vec::new();
        let cleanup =
            StorageReadControl::new(control.memory(), &uqa_core::CancellationToken::new());
        graph.write_checkpoint(&mut checkpoint, &cleanup)?;
        fixture.checkpoint = checkpoint;
        if fault == CheckpointFault::LoseReply {
            return Err(StorageBackendError::Other("injected lost checkpoint reply".into()).into());
        }
        result
    }
}

impl Persistence {
    fn checkpoint_fault(&self, after: usize, fault: CheckpointFault) {
        let mut fixture = self.serializable.lock();
        fixture.fault = (fixture.admissions + after, fault);
    }

    fn actor_status(&self, id: SerializableTransactionId) -> VersionResult<SerializableStatus> {
        let control = StorageReadControl::with_limit(1 << 24);
        self.recover_serializable_participants(&control)?;
        let mut status = None;
        self.with_serializable_admission(&control, &mut |graph, _| {
            status = Some(graph.status(id)?);
            Ok(())
        })?;
        Ok(status.unwrap())
    }
}

fn start(
    persistence: &Arc<Persistence>,
    read_only: bool,
) -> (VersionedKeyValueStore, SerializableReadContext) {
    let session = persistence.session(1 << 20);
    if read_only {
        session.begin_read_transaction().unwrap();
    } else {
        session.begin_transaction().unwrap();
    }
    let context = session.establish_serializable_snapshot().unwrap();
    (session, context)
}

fn predicate(key: &[u8]) -> SerializablePredicate<'_> {
    SerializablePredicate::point([7; 16], SerializableKeySpace::Rows, key)
}

fn read(context: &SerializableReadContext, key: &[u8]) {
    context
        .observe_read(predicate(key), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
}

fn write(session: &VersionedKeyValueStore, key: &[u8]) {
    session.observe_serializable_write(predicate(key)).unwrap();
    session.put(key, b"changed").unwrap();
}

#[test]
fn backend_capabilities_keep_original_attribution_budget_and_reader_cancellation() {
    use uqa_storage::{KeyValueStorageBackend, PersistentStorageBackend};
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let backend = KeyValueStorageBackend::new(store.clone());
    let session = backend.serializable_session().unwrap();
    assert!(session.serializable_read_context().unwrap().is_none());
    backend.begin_transaction().unwrap();
    let actor = session.establish_serializable_snapshot().unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    let reader = backend.open_retained_read_session(&cancellation).unwrap();
    let reader_session = reader.backend.serializable_session().unwrap();
    let retained = reader_session.serializable_read_context().unwrap().unwrap();
    assert_eq!(retained.id(), actor.id());
    let control = retained.read_control(&cancellation);
    assert!(control
        .memory()
        .shares_allowance(store.retention_control().memory()));
    assert!(reader_session.establish_serializable_snapshot().is_err());
    assert!(reader_session
        .observe_serializable_write(predicate(b"x"))
        .is_err());
    cancellation.cancel();
    assert!(matches!(
        retained
            .observe_read(predicate(b"x"), &control)
            .unwrap_err()
            .into_storage_error(),
        StorageBackendError::Cancelled(_)
    ));
    assert!(store.retention_control().check().is_ok());
    cancellation.reset();
    retained.observe_read(predicate(b"x"), &control).unwrap();
    reader.backend.begin_read_transaction().unwrap();
    reader.backend.commit_transaction().unwrap();
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Active
    );
    backend.commit_transaction().unwrap();
    backend.begin_transaction().unwrap();
    let next = session.establish_serializable_snapshot().unwrap();
    assert_ne!(next.id(), actor.id());
    assert_eq!(
        reader_session
            .serializable_read_context()
            .unwrap()
            .unwrap()
            .id(),
        actor.id()
    );
    backend.rollback_transaction().unwrap();
}

#[test]
fn empty_sessions_finish_without_a_physical_allocation_and_views_retain_the_original_actor() {
    for read_only in [false, true] {
        let persistence = Persistence::new();
        let (session, actor) = start(&persistence, read_only);
        let view = session.record_snapshot().unwrap();
        let nested = view.try_clone().unwrap();
        assert_eq!(nested.serializable().unwrap().id(), actor.id());
        read(nested.serializable().unwrap(), b"absent");
        session.commit_transaction().unwrap();
        assert!(!session.in_transaction());
        assert_eq!(persistence.state.lock().next, 0);
        assert_eq!(
            persistence.actor_status(actor.id()).unwrap(),
            SerializableStatus::Committed
        );
        let id = actor.id();
        drop((actor, view));
        assert_eq!(
            persistence.actor_status(id).unwrap(),
            SerializableStatus::Committed
        );
        drop(nested);
        assert!(matches!(
            persistence.actor_status(id),
            Err(VersionError::UnknownTransaction)
        ));
        assert_eq!(session.retention_control().memory().used(), 0);
    }
}

#[test]
fn retained_read_sessions_preserve_attribution_without_finishing_the_original_participant() {
    let persistence = Persistence::new();
    let (source, actor) = start(&persistence, false);
    let cancellation = uqa_core::CancellationToken::new();
    let reader = source.new_retained_read_session(&cancellation).unwrap();
    let view = reader.record_snapshot().unwrap();
    assert_eq!(view.serializable().unwrap().id(), actor.id());
    read(view.serializable().unwrap(), b"absent");
    drop(view);
    for commit in [false, true] {
        reader.begin_read_transaction().unwrap();
        if commit {
            reader.commit_transaction().unwrap();
        } else {
            reader.rollback_transaction().unwrap();
        }
        assert_eq!(
            persistence.actor_status(actor.id()).unwrap(),
            SerializableStatus::Active
        );
    }
    source.commit_transaction().unwrap();
    let id = actor.id();
    drop((actor, source));
    assert_eq!(
        persistence.actor_status(id).unwrap(),
        SerializableStatus::Committed
    );
    assert_eq!(persistence.state.lock().next, 0);
    drop(reader);
    assert!(matches!(
        persistence.actor_status(id),
        Err(VersionError::UnknownTransaction)
    ));
}

#[test]
fn first_snapshot_after_a_savepoint_and_command_refresh_keep_original_attribution() {
    let persistence = Persistence::new();
    let session = persistence.session(1 << 20);
    let peer = persistence.session(1 << 20);
    peer.put(b"x", b"before").unwrap();
    session.begin_transaction().unwrap();
    session.savepoint("before first snapshot").unwrap();
    peer.put(b"x", b"first snapshot").unwrap();
    let actor = session.establish_serializable_snapshot().unwrap();
    let view = session.record_snapshot().unwrap();
    let control = session.retention_control();
    assert_eq!(
        view.get(b"x", &control).unwrap().unwrap().value(),
        Some(b"first snapshot".as_slice())
    );
    peer.put(b"x", b"new command").unwrap();
    assert_eq!(
        session.establish_serializable_snapshot().unwrap().id(),
        actor.id()
    );
    assert_eq!(session.get(b"x").unwrap().unwrap(), b"first snapshot");
    session
        .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
        .unwrap();
    assert_eq!(session.get(b"x").unwrap().unwrap(), b"new command");
    assert_eq!(
        session
            .record_snapshot()
            .unwrap()
            .serializable()
            .unwrap()
            .id(),
        actor.id()
    );
    assert_eq!(
        view.get(b"x", &control).unwrap().unwrap().value(),
        Some(b"first snapshot".as_slice())
    );
    session
        .rollback_to_savepoint("before first snapshot")
        .unwrap();
    assert_eq!(session.get(b"x").unwrap().unwrap(), b"first snapshot");
    assert_eq!(
        session
            .record_snapshot()
            .unwrap()
            .serializable()
            .unwrap()
            .id(),
        actor.id()
    );
    session.rollback_transaction().unwrap();
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Aborted
    );
}

#[test]
fn disjoint_physical_rows_cannot_commit_a_logical_write_skew() {
    let persistence = Persistence::new();
    let (a, actor_a) = start(&persistence, false);
    let (b, actor_b) = start(&persistence, false);
    read(&actor_a, b"x");
    read(&actor_b, b"y");
    write(&a, b"y");
    write(&b, b"x");
    a.commit_transaction().unwrap();
    let error = b.commit_transaction().unwrap_err();
    assert!(
        error.transaction_outcome().is_none(),
        "dependency failure is not an uncertain commit: {error}"
    );
    assert!(error.to_string().contains("read/write dependencies"));
    b.rollback_transaction().unwrap();
    assert_eq!(persistence.session(1 << 20).get(b"x").unwrap(), None);
    assert_eq!(
        persistence.session(1 << 20).get(b"y").unwrap().unwrap(),
        b"changed"
    );
}

#[test]
fn savepoint_undo_removes_later_write_intents_but_keeps_reads() {
    for retain_read in [false, true] {
        let persistence = Persistence::new();
        let (a, actor_a) = start(&persistence, false);
        let (b, actor_b) = start(&persistence, false);
        if retain_read {
            write(&a, b"y");
        }
        a.savepoint("s").unwrap();
        read(&actor_a, b"x");
        if !retain_read {
            write(&a, b"y");
        }
        a.rollback_to_savepoint("s").unwrap();
        read(&actor_b, b"y");
        write(&b, b"x");
        a.commit_transaction().unwrap();
        if retain_read {
            assert!(b.commit_transaction().is_err());
            b.rollback_transaction().unwrap();
        } else {
            b.commit_transaction().unwrap();
            assert_eq!(a.get(b"y").unwrap(), None);
        }
    }
}

#[test]
fn a_lost_empty_commit_reply_retains_logical_identity_without_a_physical_receipt() {
    for fault in [CheckpointFault::Reject, CheckpointFault::LoseReply] {
        let persistence = Persistence::new();
        let (session, actor) = start(&persistence, true);
        persistence.checkpoint_fault(1, fault);
        let error = session.commit_transaction().unwrap_err();
        assert_eq!(
            error.transaction_outcome(),
            Some(TransactionOutcome::Indeterminate(
                TransactionOutcomeId::Serializable(actor.id())
            ))
        );
        assert_eq!(error.commit_outcome(), None);
        assert_eq!(session.pending_commit(), None);
        assert_eq!(
            session.pending_transaction_completion(),
            Some(TransactionOutcomeId::Serializable(actor.id()))
        );
        assert!(session.in_transaction());
        assert!(session.savepoint("sealed").is_err());
        session.commit_transaction().unwrap();
        assert_eq!(session.pending_transaction_completion(), None);
        assert_eq!(
            persistence.actor_status(actor.id()).unwrap(),
            SerializableStatus::Committed
        );
        assert_eq!(persistence.state.lock().next, 0);
    }
}

#[test]
fn rollback_resolves_a_lost_empty_commit_instead_of_claiming_it_was_undone() {
    for fault in [CheckpointFault::Reject, CheckpointFault::LoseReply] {
        let persistence = Persistence::new();
        let (session, actor) = start(&persistence, false);
        persistence.checkpoint_fault(1, fault);
        session.commit_transaction().unwrap_err();
        let rollback = session.rollback_transaction();
        if fault == CheckpointFault::LoseReply {
            assert_eq!(
                rollback.unwrap_err().transaction_outcome(),
                Some(TransactionOutcome::Committed(
                    TransactionOutcomeId::Serializable(actor.id())
                ))
            );
            assert!(session.in_transaction());
            session.commit_transaction().unwrap();
        } else {
            rollback.unwrap();
            assert_eq!(
                persistence.actor_status(actor.id()).unwrap(),
                SerializableStatus::Aborted
            );
        }
        assert_eq!(persistence.state.lock().next, 0);
    }
}

#[test]
fn a_preparation_checkpoint_failure_never_attempts_main_publication() {
    for fault in [CheckpointFault::Reject, CheckpointFault::LoseReply] {
        let persistence = Persistence::new();
        let (session, actor) = start(&persistence, false);
        write(&session, b"x");
        persistence.checkpoint_fault(1, fault);
        session.commit_transaction().unwrap_err();
        assert!(persistence.state.lock().attempts.is_empty());
        assert_eq!(persistence.session(1 << 20).get(b"x").unwrap(), None);
        session.rollback_transaction().unwrap();
        assert_eq!(
            persistence.actor_status(actor.id()).unwrap(),
            SerializableStatus::Aborted
        );
    }
}

#[test]
fn a_lost_main_commit_reply_reconciles_the_exact_receipt_without_republishing() {
    let persistence = Persistence::new();
    let (session, actor) = start(&persistence, false);
    write(&session, b"x");
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    let error = session.commit_transaction().unwrap_err();
    let physical = session.pending_commit().unwrap();
    assert_eq!(
        error.commit_outcome(),
        Some(CommitErrorOutcome::Indeterminate(physical))
    );
    assert_eq!(
        error.transaction_outcome(),
        Some(TransactionOutcome::Indeterminate(
            TransactionOutcomeId::Records(physical)
        ))
    );
    session.commit_transaction().unwrap();
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Committed
    );
    assert_eq!(persistence.state.lock().attempts.len(), 1);
}

#[test]
fn a_checkpoint_failure_after_main_commit_retains_known_physical_completion() {
    for fault in [CheckpointFault::Reject, CheckpointFault::LoseReply] {
        let persistence = Persistence::new();
        let (session, actor) = start(&persistence, false);
        write(&session, b"x");
        persistence.checkpoint_fault(2, fault);
        let error = session.commit_transaction().unwrap_err();
        let physical = session.pending_commit().unwrap();
        assert!(
            matches!(error.commit_outcome(), Some(CommitErrorOutcome::Committed(receipt)) if receipt.transaction == physical)
        );
        assert_eq!(
            error.transaction_outcome(),
            Some(TransactionOutcome::Committed(
                TransactionOutcomeId::Records(physical)
            ))
        );
        assert!(session
            .rollback_transaction()
            .unwrap_err()
            .transaction_outcome()
            .is_some_and(|outcome| matches!(outcome, TransactionOutcome::Committed(_))));
        session.commit_transaction().unwrap();
        assert_eq!(
            persistence.actor_status(actor.id()).unwrap(),
            SerializableStatus::Committed
        );
        assert_eq!(persistence.state.lock().attempts.len(), 1);
        assert_eq!(session.get(b"x").unwrap().unwrap(), b"changed");
    }
}

#[test]
fn an_aborted_main_receipt_cannot_be_reclassified_by_a_checkpoint_failure() {
    let persistence = Persistence::new();
    let (session, actor) = start(&persistence, false);
    write(&session, b"x");
    persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
    session.commit_transaction().unwrap_err();
    let physical = session.pending_commit().unwrap();
    persistence.checkpoint_fault(1, CheckpointFault::Reject);
    let error = session.rollback_transaction().unwrap_err();
    assert_eq!(
        error.commit_outcome(),
        Some(CommitErrorOutcome::Aborted(physical))
    );
    assert_eq!(
        error.transaction_outcome(),
        Some(TransactionOutcome::Aborted(TransactionOutcomeId::Records(
            physical
        )))
    );
    session.rollback_transaction().unwrap();
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Aborted
    );
    assert_eq!(session.get(b"x").unwrap(), None);
}

#[test]
fn cancelled_preparation_keeps_typed_cancellation_and_uncancelled_rollback() {
    let persistence = Persistence::new();
    let cancellation = uqa_core::CancellationToken::new();
    let session = VersionedKeyValueStore::new_with_cancellation(
        persistence.clone(),
        None,
        VersionedSessionOptions {
            retained_bytes: 1 << 20,
        },
        cancellation.clone(),
    );
    session.begin_transaction().unwrap();
    let actor = session.establish_serializable_snapshot().unwrap();
    write(&session, b"x");
    cancellation.cancel();
    let error = session.commit_transaction().unwrap_err();
    assert!(error.transaction_outcome().is_none());
    assert!(
        matches!(error, StorageBackendError::Cancelled(_)),
        "{error}"
    );
    assert!(persistence.state.lock().attempts.is_empty());
    session.rollback_transaction().unwrap();
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Aborted
    );
    assert_eq!(session.get(b"x").unwrap(), None);
}

#[test]
fn failed_savepoint_checkpoint_cannot_leave_undone_private_records_publishable() {
    let persistence = Persistence::new();
    let (session, actor) = start(&persistence, false);
    write(&session, b"keep");
    session.savepoint("s").unwrap();
    read(&actor, b"read after savepoint");
    write(&session, b"undo");
    persistence.checkpoint_fault(1, CheckpointFault::Reject);
    session.rollback_to_savepoint("s").unwrap_err();
    assert_eq!(session.get(b"keep").unwrap().unwrap(), b"changed");
    assert_eq!(session.get(b"undo").unwrap(), None);
    session.rollback_to_savepoint("s").unwrap();
    session.commit_transaction().unwrap();
    assert_eq!(session.get(b"keep").unwrap().unwrap(), b"changed");
    assert_eq!(session.get(b"undo").unwrap(), None);
    assert_eq!(
        persistence.actor_status(actor.id()).unwrap(),
        SerializableStatus::Committed
    );
}
