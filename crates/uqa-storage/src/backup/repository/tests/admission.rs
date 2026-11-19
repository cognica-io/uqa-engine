//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{sync::mpsc, time::Duration};

use super::*;

#[test]
fn repository_admission_preserves_a_waiting_pack_and_observes_cancellation() {
    for cancel in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            temporary.path().join("backup"),
            protected(),
            limits(),
            &control,
        )
        .unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        let mut pack = operation.begin_pack(&control).unwrap();
        let payload = b"retained synchronized pack";
        let range = pack
            .append(FrameKind::Unit, &mut Cursor::new(payload), &control)
            .unwrap();
        let gate = repository.admission(&control).unwrap();
        let (events, observed) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let result = std::thread::scope(|scope| {
            let control = &control;
            let worker = scope.spawn(move || {
                let notice = events.clone();
                directory::ON_ADMISSION_CONTENTION.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || {
                        notice.send("contended").unwrap();
                        resumed.recv_timeout(Duration::from_secs(10)).unwrap();
                    }));
                });
                let result = pack.finish(control).and_then(|object| object.span(range));
                events.send("finished").unwrap();
                result
            });
            let first = observed.recv_timeout(Duration::from_secs(10)).unwrap();
            if cancel {
                control.cancellation().cancel();
                // Keep the competing lease held until cancellation has returned.
                let _ = resume.send(());
                let result = worker.join().unwrap();
                drop(gate);
                assert_eq!(first, "contended", "finalization: {result:?}");
                result
            } else {
                drop(gate);
                let _ = resume.send(());
                let result = worker.join().unwrap();
                assert_eq!(first, "contended", "finalization: {result:?}");
                result
            }
        });
        if cancel {
            assert!(matches!(result, Err(BackupError::Cancelled(_))));
            assert!(matches!(
                operation.begin_pack(&control),
                Err(BackupError::Finished)
            ));
            assert!(operation.cancel().unwrap().is_none());
        } else {
            let span = result.unwrap();
            let name = directory::name(span.object().as_bytes());
            let mut input =
                directory::open_file(&repository.objects, directory::text(&name), false).unwrap();
            let decoded = read_frame(
                &mut input,
                FrameContext {
                    repository: repository.id(),
                    object: span.object(),
                    offset: span.offset(),
                    kind: FrameKind::Unit,
                },
                &repository.protection,
                limits(),
                &control,
            )
            .unwrap();
            assert_eq!(&*decoded, payload);
            let expected = prepare(&repository, &mut operation, &control);
            assert_eq!(operation.publish(&control).unwrap(), expected);
            repository
                .open_backup(operation.id(), &control)
                .unwrap()
                .verify(1024 * 1024, &control)
                .unwrap();
        }
    }
}
