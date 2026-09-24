//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{CommitReceipt, CommitSequence};

mod completion;

fn setup() -> (
    SerializableGraph,
    Arc<LocalSerializableLeases>,
    StorageReadControl,
) {
    let control = StorageReadControl::with_limit(1 << 20);
    let graph =
        SerializableGraph::new(DatabaseId::from_bytes([9; 16]), [4; 16], control.memory()).unwrap();
    let leases = Arc::new(LocalSerializableLeases::new(control.memory()));
    (graph, leases, control)
}

#[test]
fn nested_clones_keep_participants_live_and_drops_never_enter_recovery() {
    let (mut graph, leases, control) = setup();
    let actor = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    let id = actor.id();
    let nested = actor.clone();
    drop(actor);
    graph
        .recover_abandoned(
            &control,
            |id| Ok(leases.is_alive(id)),
            |_| panic!("live participant"),
        )
        .unwrap();
    graph.check_active(id).unwrap();
    drop(nested);
    graph
        .recover_abandoned(
            &control,
            |id| Ok(leases.is_alive(id)),
            |_| panic!("unbound participant"),
        )
        .unwrap();
    assert!(matches!(
        graph.check_active(id),
        Err(VersionError::TransactionFinished)
    ));
    graph.reclaim();
    leases.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn dead_publishers_resolve_physical_receipts_and_unknown_state_stays_prepared() {
    let (mut graph, leases, control) = setup();
    let mut publications = Vec::new();
    for allocation in [101, 102, 103] {
        let actor = graph
            .admit_with_lease(false, &control, |id| leases.retain(id, &control))
            .unwrap();
        let physical = StorageTransactionId::new(graph.database(), allocation).unwrap();
        publications.push(
            graph
                .prepare_publication(actor.id(), physical, [2; 32], &control)
                .unwrap(),
        );
    }
    let committed = CommitReceipt {
        transaction: publications[0].transaction(),
        fingerprint: [2; 32],
        sequence: CommitSequence::from_u64(7),
    };
    let result = graph.recover_abandoned(
        &control,
        |id| Ok(leases.is_alive(id)),
        |id| match id.allocation() {
            101 => Err(VersionError::AlreadyCommitted(committed)),
            102 => Ok(CommitStatus::Aborted),
            103 => Ok(CommitStatus::Unknown),
            _ => panic!("SSI allocation was used as a physical receipt key"),
        },
    );
    assert!(matches!(result, Err(VersionError::UnknownTransaction)));
    assert_eq!(
        graph
            .resolve_publication(publications[0], CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Committed(committed)
    );
    assert_eq!(
        graph
            .resolve_publication(publications[1], CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Aborted
    );
    assert!(matches!(
        graph.check_active(publications[2].participant()),
        Err(VersionError::TransactionSealed)
    ));
    graph
        .recover_abandoned(&control, |_| Ok(false), |_| Ok(CommitStatus::Pending))
        .unwrap();
    assert!(matches!(
        graph.check_active(publications[2].participant()),
        Err(VersionError::TransactionSealed)
    ));
}

#[test]
fn untracked_owners_survive_recovery_and_lease_bindings_survive_checkpoints() {
    let (mut graph, leases, control) = setup();
    let manual = graph.admit(false, &control).unwrap();
    let actor = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    let id = actor.id();
    let mut checkpoint = Vec::new();
    graph.write_checkpoint(&mut checkpoint, &control).unwrap();
    let database = graph.database();
    let coordinator = graph.coordinator();
    drop(graph);
    drop(actor);
    let mut graph = SerializableGraph::read_checkpoint(
        database,
        coordinator,
        &mut checkpoint.as_slice(),
        &control,
    )
    .unwrap();
    graph
        .recover_abandoned(
            &control,
            |_| Ok(false),
            |_| panic!("no physical publication"),
        )
        .unwrap();
    graph.check_active(manual).unwrap();
    assert!(matches!(
        graph.check_active(id),
        Err(VersionError::TransactionFinished)
    ));
}

#[test]
fn failed_or_mismatched_lease_admission_cannot_leave_an_active_actor() {
    let (mut graph, leases, control) = setup();
    assert!(graph
        .admit_with_lease(false, &control, |_| Err(VersionError::InvalidTransactionId))
        .is_err());
    let foreign = SerializableTransactionId::new(graph.database(), [6; 16], 1).unwrap();
    assert!(graph
        .admit_with_lease(false, &control, |_| leases.retain(foreign, &control))
        .is_err());
    graph.reclaim();
    leases.reclaim();
    assert_eq!(control.memory().used(), 0);
    assert_eq!(graph.admit(true, &control).unwrap().allocation(), 3);
}

#[test]
fn legacy_checkpoints_cannot_enable_lease_recovery() {
    use sha2::{Digest, Sha256};
    let (mut graph, _, control) = setup();
    let actor = graph.admit(false, &control).unwrap();
    let mut bytes = Vec::new();
    graph.write_checkpoint(&mut bytes, &control).unwrap();
    bytes[..8].copy_from_slice(b"UQASER01");
    let footer = bytes.len() - 32;
    let checksum = Sha256::digest(&bytes[..footer]);
    bytes[footer..].copy_from_slice(&checksum);
    let mut restored = SerializableGraph::read_checkpoint(
        graph.database(),
        graph.coordinator(),
        &mut bytes.as_slice(),
        &control,
    )
    .unwrap();
    restored
        .recover_abandoned(&control, |_| Ok(false), |_| panic!("legacy actor"))
        .unwrap();
    restored.check_active(actor).unwrap();
}

#[test]
fn an_empty_registry_does_not_retain_a_failed_callers_allowance() {
    let empty = StorageReadControl::with_limit(0);
    let leases = Arc::new(LocalSerializableLeases::new(empty.memory()));
    let (mut graph, _, control) = setup();
    let actor = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    assert!(leases.is_alive(actor.id()));
    assert_eq!(empty.memory().used(), 0);
    drop(actor);
    leases.reclaim();
    let tiny = StorageReadControl::with_limit(1);
    let id = SerializableTransactionId::new(graph.database(), graph.coordinator(), 100).unwrap();
    assert!(matches!(
        leases.retain(id, &tiny),
        Err(VersionError::Memory(_))
    ));
    let next = leases.retain(id, &control).unwrap();
    assert!(leases.is_alive(next.id()));
    assert_eq!(tiny.memory().used(), 0);
}

#[test]
fn final_local_lease_release_returns_registry_capacity_without_another_operation() {
    let control = StorageReadControl::with_limit(4096);
    let leases = Arc::new(LocalSerializableLeases::new(control.memory()));
    let id = SerializableTransactionId::new(DatabaseId::from_bytes([1; 16]), [2; 16], 1).unwrap();
    let actor = leases.retain(id, &control).unwrap();
    let nested = actor.clone();
    let retained = control.memory().used();
    drop(actor);
    assert_eq!(control.memory().used(), retained);
    drop(nested);
    assert_eq!(
        control.memory().used(),
        0,
        "last owner must release the registry entry and its empty capacity"
    );
}

#[test]
fn failed_local_lease_publication_returns_every_admitted_buffer() {
    let payload =
        size_of::<LocalLeaseOwner<()>>() + size_of::<Participant>() + 2 * size_of::<usize>();
    for limit in [
        0,
        payload - 1,
        payload,
        payload + size_of::<LocalEntry>() - 1,
    ] {
        let control = StorageReadControl::with_limit(limit);
        let leases = Arc::new(LocalSerializableLeases::new(control.memory()));
        let id =
            SerializableTransactionId::new(DatabaseId::from_bytes([1; 16]), [2; 16], 1).unwrap();
        assert!(leases.retain(id, &control).is_err(), "limit {limit}");
        assert_eq!(
            control.memory().used(),
            0,
            "failed admission at limit {limit}"
        );
    }
}
