//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn path_watch_detects_leaf_and_ancestor_changes_without_timing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    for ancestor in [false, true] {
        let parent = root.join(if ancestor { "ancestor" } else { "leaf" });
        std::fs::create_dir_all(parent.join("nested")).unwrap();
        let path = parent.join("nested/db");
        std::fs::write(&path, b"original").unwrap();
        let mut watch = match PathChangeWatch::new(&path) {
            Ok(watch) => watch,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("watch creation failed: {error}"),
        };
        for _ in 0..128 {
            assert!(!watch.changed().unwrap());
        }
        if ancestor {
            std::fs::rename(&parent, root.join("moved")).unwrap();
        } else {
            std::fs::rename(&path, path.with_extension("moved")).unwrap();
        }
        assert!(watch.changed().unwrap());
        assert!(watch.changed().unwrap(), "invalidation stays set");
    }
}

#[test]
fn path_watch_rejects_symlink_ancestors() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("actual")).unwrap();
    std::os::unix::fs::symlink(root.join("actual"), root.join("alias")).unwrap();
    assert!(PathChangeWatch::new(&root.join("alias/db")).is_err());
}
