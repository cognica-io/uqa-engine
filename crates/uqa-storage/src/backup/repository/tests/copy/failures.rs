//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{sync::mpsc, time::Duration};

#[test]
fn standalone_copy_rejects_corruption_even_in_unselected_inherited_pack_frames() {
    for damage in 0..4 {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let source = repository(temporary.path().join("source"), 1, &control);
        let original = vec![Unit::record(0, &[1; 256]), Unit::record(1, &[2; 256])];
        let (parent, _) = capture(&source, None, original.clone(), &control, 1 << 20);
        let spans = read(&source, parent, &control);
        assert_eq!(spans[0].1.object(), spans[1].1.object());
        let (child, report) = capture(
            &source,
            Some(parent.backup()),
            original[..1].to_vec(),
            &control,
            1 << 20,
        );
        assert_eq!(report.image.reused_units, 1);
        let lease = source.open_backup(child.backup(), &control).unwrap();
        let span = match damage {
            0 => spans[1].1,
            1 | 3 => spans[0].1,
            2 => child.inventory(),
            _ => unreachable!(),
        };
        let name = directory::name(span.object().as_bytes());
        if damage == 3 {
            source.objects.remove_file(directory::text(&name)).unwrap();
        } else {
            let mut file =
                directory::open_file(&source.objects, directory::text(&name), true).unwrap();
            file.seek(SeekFrom::Start(span.offset() + span.encoded_bytes() / 2))
                .unwrap();
            file.write_all(b"corruption").unwrap();
            file.sync_all().unwrap();
        }
        if damage == 0 {
            let mut input = lease.read_image(1 << 20, &control).unwrap();
            assert_eq!(input.advance(4096).unwrap(), BackupReadStep::Unit);
            let mut payload = [0; 256];
            assert_eq!(input.read_current(&mut payload).unwrap(), payload.len());
            assert_eq!(payload, [1; 256]);
            assert_eq!(input.read_current(&mut payload).unwrap(), 0);
        }
        let destination = repository(temporary.path().join("destination"), 2, &control);
        let mut copy = destination
            .begin_backup(child.cut(), None, 1 << 20, &control)
            .unwrap();
        assert!(copy.prepare_copy(&lease, 1 << 20, &control).is_err());
        assert!(matches!(copy.publish(&control), Err(BackupError::Finished)));
        let id = copy.id();
        assert_eq!(control.memory().used(), 0);
        drop(copy);
        assert!(destination.open_backup(id, &control).is_err());
        assert!(destination
            .resume_publication(id, 1 << 20, &control)
            .is_err());
    }
}

#[test]
fn standalone_copy_invalid_intents_and_limits_cannot_publish_or_continue() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let source = repository(temporary.path().join("source"), 1, &control);
    let (root, _) = capture(
        &source,
        None,
        vec![Unit::record(0, &[7; 20000])],
        &control,
        1 << 20,
    );
    let lease = source.open_backup(root.backup(), &control).unwrap();
    let destination = repository(temporary.path().join("destination"), 2, &control);
    let (parent, _) = capture(
        &destination,
        None,
        vec![Unit::record(0, b"existing")],
        &control,
        1 << 20,
    );
    for fault in 0..7 {
        let target = if fault == 0 { &source } else { &destination };
        let cut = if fault == 1 { cut() } else { root.cut() };
        let parent = (fault == 2).then_some(parent.backup());
        let staging = if fault == 3 { 1024 } else { 1 << 20 };
        let workspace = if fault == 4 { 1 } else { 1 << 20 };
        let mut copy = target.begin_backup(cut, parent, staging, &control).unwrap();
        if fault == 5 {
            *target.fault.lock() = Some(Fault::ObjectSync);
        }
        let read_control = if fault == 6 {
            StorageReadControl::with_limit(1)
        } else {
            control.clone()
        };
        assert!(copy.prepare_copy(&lease, workspace, &read_control).is_err());
        assert!(matches!(
            copy.prepare_copy(&lease, 1 << 20, &control),
            Err(BackupError::Finished)
        ));
        assert!(matches!(copy.publish(&control), Err(BackupError::Finished)));
        assert_eq!(copy.cancel().unwrap(), None);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(read_control.memory().used(), 0);
    }
    destination
        .open_backup(parent.backup(), &control)
        .unwrap()
        .verify(1 << 20, &control)
        .unwrap();
    lease.verify(1 << 20, &control).unwrap();
}

#[test]
fn standalone_copy_retains_the_source_lease_and_cancels_while_destination_admission_waits() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let source = repository(temporary.path().join("source"), 1, &control);
    let (root, _) = capture(
        &source,
        None,
        vec![Unit::record(0, &[7; 20000])],
        &control,
        1 << 20,
    );
    let lease = source.open_backup(root.backup(), &control).unwrap();
    let destination = repository(temporary.path().join("destination"), 2, &control);
    let mut copy = destination
        .begin_backup(root.cut(), None, 1 << 20, &control)
        .unwrap();
    let gate = destination.admission(&control).unwrap();
    let cancellation = CancellationToken::new();
    let copying = StorageReadControl::new(control.memory(), &cancellation);
    let (events, observed) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    std::thread::scope(|scope| {
        let lease = &lease;
        let worker = scope.spawn(move || {
            directory::ON_ADMISSION_CONTENTION.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    events.send(()).unwrap();
                    resumed.recv_timeout(Duration::from_secs(10)).unwrap();
                }));
            });
            let result = copy.prepare_copy(lease, 1 << 20, &copying);
            (copy, result)
        });
        observed.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(
            source.resume_publication(root.backup(), 1 << 20, &control),
            Err(BackupError::Busy)
        ));
        cancellation.cancel();
        resume.send(()).unwrap();
        let (mut copy, result) = worker.join().unwrap();
        assert!(result.is_err());
        assert!(matches!(copy.publish(&control), Err(BackupError::Finished)));
        drop(gate);
        assert_eq!(copy.cancel().unwrap(), None);
    });
    assert_eq!(control.memory().used(), 0);
    lease.verify(1 << 20, &control).unwrap();
}
