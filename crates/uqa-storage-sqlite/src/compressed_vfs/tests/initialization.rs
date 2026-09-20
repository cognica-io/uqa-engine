//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An opener of an empty file follows its first publisher without weakening existing identity checks.

use super::*;

#[test]
fn empty_openers_adopt_the_first_published_identity_and_salt() {
    for encrypted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("first-publication.db");
        let mut options = encrypted_options();
        if !encrypted {
            options.key = None;
        }
        let mut waiting = ContainerFile::open(path.clone(), options.clone()).unwrap();
        let mut writer = ContainerFile::open(path, options).unwrap();
        assert_ne!(waiting.file_id, writer.file_id);
        writer.write_at(0, b"first").unwrap();
        writer.flush().unwrap();
        waiting.refresh_committed_state().unwrap();
        assert_eq!(
            waiting.authenticated_anchor(),
            writer.authenticated_anchor()
        );
        assert!(waiting.initial_key.is_none());
        assert!(writer.initial_key.is_none());
        let mut bytes = [0; 5];
        waiting.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"first");
        writer.write_at(0, b"later").unwrap();
        writer.flush().unwrap();
        waiting.refresh_committed_state().unwrap();
        waiting.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"later");
    }
}

#[test]
fn first_publication_requires_the_original_credential_without_partial_adoption() {
    for plaintext in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credential.db");
        let options = encrypted_options();
        let mut waiting = ContainerFile::open(path.clone(), options.clone()).unwrap();
        let original = waiting.authenticated_anchor();
        let mut foreign = options;
        foreign.key = (!plaintext).then(|| "different-credential".to_owned());
        let mut writer = ContainerFile::open(path, foreign).unwrap();
        writer.write_at(0, b"untrusted").unwrap();
        writer.flush().unwrap();
        assert!(waiting.refresh_committed_state().is_err());
        assert_eq!(waiting.authenticated_anchor(), original);
        assert!(waiting.initial_key.is_some());
        assert_eq!(waiting.logical_len, 0);
    }
}

#[test]
fn a_loaded_empty_header_keeps_its_identity_even_before_the_first_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bound-empty.db");
    let options = encrypted_options();
    let original = ContainerFile::open(path.clone(), options.clone()).unwrap();
    let mut file = File::create(&path).unwrap();
    original.ensure_header(&mut file, true).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let mut reader = ContainerFile::open(path.clone(), options.clone()).unwrap();
    assert_eq!(reader.generation, 0);
    assert_eq!(reader.file_id, original.file_id);
    let replacement_path = directory.path().join("replacement.db");
    let mut replacement = ContainerFile::open(replacement_path.clone(), options).unwrap();
    replacement.write_at(0, b"replacement").unwrap();
    replacement.flush().unwrap();
    std::fs::rename(replacement_path, path).unwrap();
    assert!(reader.refresh_committed_state().is_err());
    assert_eq!(reader.file_id, original.file_id);
}
