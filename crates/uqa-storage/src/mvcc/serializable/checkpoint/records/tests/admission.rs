//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission retains the first decode failure and the caller's complete memory allowance.

use std::io::Read;

use super::*;

fn fixture() -> Records {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    let actor = graph.admit(false, &control).unwrap();
    graph
        .observe_read(actor, point(&[3; 4096]), &control)
        .unwrap();
    let mut records = Records::new();
    publish(&graph, &mut records, &control);
    records
}

struct Unreadable;

impl Read for Unreadable {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("a failed admission must not read another record")
    }
}

#[test]
fn failed_record_cannot_be_replaced_by_a_later_valid_checkpoint() {
    let records = fixture();
    let mut corrupt = records[&SerializableCheckpointKey::HEADER].clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let control = StorageReadControl::with_limit(128 << 10);
    let result =
        SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, &control, |visit| {
            assert!(visit(
                SerializableCheckpointKey::HEADER.as_bytes(),
                &mut corrupt.as_slice()
            )
            .is_err());
            for (key, value) in &records {
                let _ = visit(key.as_bytes(), &mut value.as_slice());
            }
            Ok(())
        });
    assert!(matches!(result, Err(VersionError::InvalidEncoding(_))));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ignored_extra_record_invalidates_an_otherwise_complete_checkpoint() {
    let records = fixture();
    let control = StorageReadControl::with_limit(128 << 10);
    let result =
        SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, &control, |visit| {
            for (key, value) in &records {
                visit(key.as_bytes(), &mut value.as_slice())?;
            }
            assert!(visit(
                SerializableCheckpointKey::HEADER.as_bytes(),
                &mut Unreadable
            )
            .is_err());
            Ok(())
        });
    assert!(matches!(result, Err(VersionError::InvalidEncoding(_))));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn first_decode_error_survives_scanner_failure_and_reentry_without_more_reads() {
    let records = fixture();
    let header = &records[&SerializableCheckpointKey::HEADER];
    for cause in 0..4 {
        let control = StorageReadControl::with_limit(128 << 10);
        let occupied =
            (cause == 0).then(|| control.memory().reserve(control.memory().limit()).unwrap());
        let result =
            SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, &control, |visit| {
                let mut bytes = header.clone();
                match cause {
                    1 => control.cancellation().cancel(),
                    2 => bytes[8] ^= 1,
                    3 => bytes.clear(),
                    _ => {}
                }
                assert!(visit(
                    SerializableCheckpointKey::HEADER.as_bytes(),
                    &mut bytes.as_slice()
                )
                .is_err());
                control.cancellation().reset();
                assert!(visit(
                    SerializableCheckpointKey::HEADER.as_bytes(),
                    &mut Unreadable
                )
                .is_err());
                Err(VersionError::UnknownTransaction)
            });
        let error = result.err().expect("failed record admission");
        match cause {
            0 => assert!(matches!(error, VersionError::Memory(_)), "{error}"),
            1 => assert!(matches!(
                error.into_storage_error(),
                crate::StorageBackendError::Cancelled(_)
            )),
            2 => assert!(matches!(error, VersionError::WrongDatabase), "{error}"),
            _ => assert!(matches!(error, VersionError::Storage(_)), "{error}"),
        }
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn cancellation_at_scanner_completion_prevents_graph_admission() {
    let records = fixture();
    let control = StorageReadControl::with_limit(128 << 10);
    let result =
        SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, &control, |visit| {
            for (key, value) in &records {
                visit(key.as_bytes(), &mut value.as_slice())?;
            }
            control.cancellation().cancel();
            Ok(())
        });
    let error = result.err().expect("cancelled checkpoint admission");
    assert!(matches!(
        error.into_storage_error(),
        crate::StorageBackendError::Cancelled(_)
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn scanner_failure_after_complete_records_preserves_its_diagnostic() {
    let records = fixture();
    let control = StorageReadControl::with_limit(128 << 10);
    let result =
        SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, &control, |visit| {
            for (key, value) in &records {
                visit(key.as_bytes(), &mut value.as_slice())?;
            }
            Err(VersionError::UnknownTransaction)
        });
    assert!(matches!(result, Err(VersionError::UnknownTransaction)));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn decoded_state_and_inventory_share_the_original_allowance_until_drop() {
    let records = fixture();
    let control = StorageReadControl::with_limit(128 << 10);
    let graph = restore(&records, &control).unwrap();
    let retained = control.memory().used();
    assert!(retained > 4096);
    assert_eq!(control.memory().peak(), retained);
    drop(graph);
    assert_eq!(control.memory().used(), 0);

    let bounded = StorageReadControl::with_limit(retained);
    let occupied = bounded.memory().reserve(1).unwrap();
    assert!(matches!(
        restore(&records, &bounded),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(bounded.memory().used(), 1);
    drop(occupied);
    let graph = restore(&records, &bounded).unwrap();
    assert_eq!(bounded.memory().used(), retained);
    assert!(!graph.checkpoint_records_changed());
    drop(graph);
    assert_eq!(bounded.memory().used(), 0);
}
