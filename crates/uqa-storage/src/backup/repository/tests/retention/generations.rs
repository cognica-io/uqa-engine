//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Seek, SeekFrom, Write};

#[test]
fn preparation_preserves_verified_references_across_unrelated_publications() {
    for publish in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = Arc::new(repository(temporary.path().join("repository"), &control));
        let units = vec![Unit::record(0, b"retained")];
        let (old, _) = capture(&repository, None, units.clone(), &control, 1 << 20);
        let mut operation = repository
            .begin_backup(old.cut(), None, 1 << 20, &control)
            .unwrap();
        let changed = repository.clone();
        let token = control.clone();
        super::super::super::operation::ON_PREPARED_VERIFIED.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if publish {
                    capture(
                        &changed,
                        None,
                        vec![Unit::record(1, b"independent")],
                        &token,
                        1 << 20,
                    );
                } else {
                    drop(changed.begin_backup(cut(), None, 1 << 20, &token).unwrap());
                }
            }));
        });
        operation
            .prepare(old.inventory(), old.summary(), &control)
            .unwrap();
        let root = operation.publish(&control).unwrap();
        drop(operation);
        assert_eq!(root.summary(), old.summary());
        assert_eq!(
            read(&repository, root, &control)
                .into_iter()
                .map(|entry| entry.0)
                .collect::<Vec<_>>(),
            units
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn repository_generation_upgrade_preserves_removal_identity_and_strict_decoding() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("repository");
    let control = control();
    let repository = repository(&path, &control);
    let original = repository.generation(&control).unwrap();
    let mut generation = directory::open_file(&repository.root, "generation", true).unwrap();
    generation.set_len(24).unwrap();
    generation.seek(SeekFrom::Start(0)).unwrap();
    generation.write_all(b"UQAGEN01").unwrap();
    generation.write_all(&original).unwrap();
    generation.sync_all().unwrap();
    drop(generation);
    let reopened = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    assert_eq!(reopened.removal_generation(&control).unwrap(), original);
    drop(
        reopened
            .begin_backup(cut(), None, 1 << 20, &control)
            .unwrap(),
    );
    assert_ne!(reopened.generation(&control).unwrap(), original);
    assert_eq!(reopened.removal_generation(&control).unwrap(), original);
    let mutation = reopened.generation(&control).unwrap();
    let operation = reopened
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    let id = operation.id();
    drop(operation);
    reopened.remove_backup(id, &control).unwrap();
    assert_ne!(reopened.generation(&control).unwrap(), mutation);
    assert_ne!(reopened.removal_generation(&control).unwrap(), original);
    let bytes = std::fs::read(path.join("generation")).unwrap();
    assert_eq!(bytes.len(), 40);
    for invalid in [
        bytes[..24].to_vec(),
        [bytes.as_slice(), &[0]].concat(),
        [&bytes[..24], &[0; 16]].concat(),
    ] {
        std::fs::write(path.join("generation"), invalid).unwrap();
        assert!(LocalRepository::open(&path, protected(), limits(), &control).is_err());
    }
    std::fs::write(path.join("generation"), &bytes).unwrap();
    LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
}
