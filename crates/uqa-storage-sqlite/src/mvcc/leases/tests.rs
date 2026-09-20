//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace and slot reuse must not revive an old participant or release a live peer.

use super::*;

fn namespace(incarnation: u8) -> LeaseNamespace {
    LeaseNamespace {
        magic: *b"UQASSL01",
        database: DatabaseId::from_bytes([2; 16]),
        incarnation: Some([incarnation; 16]),
    }
}

fn tags(file: &NativeLeaseFile, control: &StorageReadControl) -> Vec<u64> {
    let mut tags = Vec::new();
    file.visit(control, &mut |tag| {
        tags.push(tag);
        Ok(())
    })
    .unwrap();
    tags.sort_unstable();
    tags
}

#[test]
fn reusing_a_released_slot_keeps_only_the_new_tag_and_live_peers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("leases");
    let control = StorageReadControl::with_limit(1 << 20);
    let file = NativeLeaseFile::open(&path, namespace(1)).unwrap();
    let admission = file.admit(&control).unwrap();
    let first = file.retain(11, &control).unwrap();
    let peer = file.retain(22, &control).unwrap();
    drop(admission);
    let second = NativeLeaseFile::open(&path, namespace(1)).unwrap();
    assert!(file.shares_descriptor(&second));
    drop(file);
    let _admission = second.admit(&control).unwrap();
    drop(first);
    let replacement = second.retain(33, &control).unwrap();
    assert_eq!(tags(&second, &control), [22, 33]);
    let mut retained_peer = Some(peer);
    second
        .visit(&control, &mut |tag| {
            if tag == 22 {
                drop(retained_peer.take());
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(tags(&second, &control), [33]);
    drop(replacement);
    assert!(tags(&second, &control).is_empty());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_new_incarnation_cannot_replace_a_live_namespace_or_inherit_old_tags() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("leases");
    let control = StorageReadControl::with_limit(1 << 20);
    let file = NativeLeaseFile::open(&path, namespace(1)).unwrap();
    let admission = file.admit(&control).unwrap();
    let retained = file.retain(11, &control).unwrap();
    drop((admission, file));
    assert!(matches!(
        NativeLeaseFile::open(&path, namespace(2)),
        Err(VersionError::WrongDatabase)
    ));
    drop(retained);
    let replacement = NativeLeaseFile::open(&path, namespace(2)).unwrap();
    let _admission = replacement.admit(&control).unwrap();
    assert!(tags(&replacement, &control).is_empty());
    let retained = replacement.retain(11, &control).unwrap();
    assert_eq!(tags(&replacement, &control), [11]);
    drop(retained);
    assert!(tags(&replacement, &control).is_empty());
}
