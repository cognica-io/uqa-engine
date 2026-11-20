//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn exclusive_directory_rename_never_replaces_an_existing_entry() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    directory.create_dir("private").unwrap();
    let private = directory.open_dir("private").unwrap();
    private.create_dir("source").unwrap();
    private.write("source/content", b"source bytes").unwrap();
    private.write("published", b"private entry").unwrap();
    directory.create_dir("empty").unwrap();
    directory.create_dir("populated").unwrap();
    directory
        .write("populated/content", b"existing bytes")
        .unwrap();
    directory.write("file", b"existing file").unwrap();
    for name in ["empty", "populated", "file"] {
        assert!(rename_new(&private, "source".as_ref(), &directory, name.as_ref()).is_err());
        assert_eq!(private.read("source/content").unwrap(), b"source bytes");
    }
    assert_eq!(
        directory.read("populated/content").unwrap(),
        b"existing bytes"
    );
    assert_eq!(directory.read("file").unwrap(), b"existing file");
    #[cfg(unix)]
    {
        directory.symlink("missing", "link").unwrap();
        assert!(rename_new(&private, "source".as_ref(), &directory, "link".as_ref()).is_err());
        assert!(directory.symlink_metadata("link").unwrap().is_symlink());
    }
    let before = identity(&private.open_dir("source").unwrap()).unwrap();
    rename_new(
        &private,
        "source".as_ref(),
        &directory,
        "published".as_ref(),
    )
    .unwrap();
    assert_eq!(
        identity(&directory.open_dir("published").unwrap()).unwrap(),
        before
    );
    assert_eq!(
        directory.read("published/content").unwrap(),
        b"source bytes"
    );
    assert!(private.symlink_metadata("source").is_err());
    assert_eq!(private.read("published").unwrap(), b"private entry");
}

#[test]
fn exclusive_directory_rename_rejects_paths_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    directory.create_dir("source").unwrap();
    for name in [
        "",
        ".",
        "..",
        "../outside",
        "nested/name",
        "/absolute",
        "nul\0byte",
    ] {
        assert!(rename_new(&directory, "source".as_ref(), &directory, name.as_ref()).is_err());
        assert!(rename_new(&directory, name.as_ref(), &directory, "target".as_ref()).is_err());
    }
    assert!(directory.symlink_metadata("source").is_ok());
    assert!(directory.symlink_metadata("target").is_err());
}
