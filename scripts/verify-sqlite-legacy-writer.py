#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify SQLite Key/Value migration with the actual released 0.3.6 library."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
OPEN = r'''
fn open(mode: &str, path: &std::path::Path) -> ManagedConnection {
    match mode {
        "plain" => ManagedConnection::open(path),
        "encrypted" => ManagedConnection::open_encrypted(path, "legacy fixture key"),
        "compressed" => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        "compressed-encrypted" => ManagedConnection::open_compressed_encrypted(path, "legacy fixture key", SQLiteCompressionOptions::default()),
        _ => panic!("unknown mode"),
    }.unwrap()
}
'''
OLD = r'''
use old_storage::KeyValueStore;
use old_provider::{ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStore};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let opened = SQLiteKeyValueStore::new(open(&args[1], std::path::Path::new(&args[3])));
    if args[2] == "reject" {
        match opened {
            Err(error) => {
                let message = error.to_string();
                assert!(message.contains("_key_value"), "unrelated open failure: {message}");
            }
            Ok(store) => {
                assert!(store.get(b"a\0\xff").is_err());
                store.begin_transaction().unwrap();
                assert!(store.put(b"forbidden", b"old writer").is_err());
                store.rollback_transaction().unwrap();
                assert!(store.delete_prefix(b"").is_err());
                let mut batch = store.batch();
                batch.put(b"forbidden", b"old batch").unwrap();
                assert!(batch.commit().is_err());
            }
        }
        assert!(old_provider::Catalog::open(open(&args[1], std::path::Path::new(&args[3]))).is_err(), "released native catalog accepted a versioned KeyValue file");
        println!("Released writer rejected for {}", args[1]);
        return;
    }
    let store = opened.unwrap();
    store.begin_transaction().unwrap();
    store.put(b"", b"").unwrap();
    store.put(b"a\0\xff", b"original\0\xff").unwrap();
    store.savepoint("keep").unwrap();
    store.put(b"discarded", b"discarded").unwrap();
    store.rollback_to_savepoint("keep").unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(store.get(b"a\0\xff").unwrap().unwrap(), b"original\0\xff");
}
'''
NEW = r'''
use new_storage::KeyValueStore;
use new_provider::{ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStore};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let store = SQLiteKeyValueStore::new(open(&args[1], std::path::Path::new(&args[3]))).unwrap();
    assert_eq!(store.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(store.get(b"a\0\xff").unwrap().unwrap(), b"original\0\xff");
    assert_eq!(store.get(b"discarded").unwrap(), None);
    assert_eq!(store.get(b"forbidden").unwrap(), None);
    if args[2] == "migrate" {
        assert_eq!(store.change_version().unwrap(), Some(1));
        store.put(b"new", b"after migration").unwrap();
    } else {
        assert_eq!(store.get(b"new").unwrap().unwrap(), b"after migration");
    }
    assert_eq!(store.change_version().unwrap(), Some(2));
}
'''


def main() -> None:
    target = ROOT / "target" / "sqlite-legacy-probe"
    with tempfile.TemporaryDirectory(prefix="uqa-sqlite-legacy-") as temporary:
        project = Path(temporary)
        (project / "src" / "bin").mkdir(parents=True)
        manifest = f'''[package]
name = "uqa-sqlite-legacy-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
rusqlite = {{ version = "=0.39.0", default-features = false }}
old_provider = {{ package = "uqa-storage-sqlite", version = "=0.3.6" }}
old_storage = {{ package = "uqa-storage", version = "=0.3.6" }}
new_provider = {{ package = "uqa-storage-sqlite", path = {json.dumps(str(ROOT / "crates/uqa-storage-sqlite"))} }}
new_storage = {{ package = "uqa-storage", path = {json.dumps(str(ROOT / "crates/uqa-storage"))} }}
'''
        (project / "Cargo.toml").write_text(manifest)
        (project / "src" / "bin" / "old.rs").write_text(OLD + OPEN)
        (project / "src" / "bin" / "new.rs").write_text(NEW + OPEN)
        subprocess.run(
            ["cargo", "build", "--manifest-path", str(project / "Cargo.toml"), "--target-dir", str(target)],
            check=True,
            env=dict(os.environ, CARGO_BUILD_JOBS="2"),
        )
        packages = tomllib.loads((project / "Cargo.lock").read_text())["package"]
        released = [package for package in packages if package["name"] == "uqa-storage-sqlite" and package.get("source", "").startswith("registry+")]
        assert len(released) == 1 and released[0]["version"] == "0.3.6", released
        print(f"Released provider checksum: {released[0]['checksum']}", flush=True)
        extension = ".exe" if os.name == "nt" else ""
        old = str(target / "debug" / f"old{extension}")
        new = str(target / "debug" / f"new{extension}")
        for mode in ("plain", "encrypted", "compressed", "compressed-encrypted"):
            database = str(project / f"{mode}.db")
            for binary, action in ((old, "create"), (new, "migrate"), (old, "reject"), (new, "reopen")):
                subprocess.run([binary, mode, action, database], check=True)
        print("Actual 0.3.6 create, migration, old-writer rejection and reopen passed in four modes.")


if __name__ == "__main__":
    main()
