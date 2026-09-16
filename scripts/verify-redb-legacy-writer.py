#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise migration and writer rejection with the actual crates.io 0.3.6 provider."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile

from cargo_package_provenance import registry_package_checksum


ROOT = Path(__file__).resolve().parents[1]
OLD = r'''
use old_storage::KeyValueStore;
use old_provider::RedbStorage;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let path = &args[2];
    if args[1] == "reject" {
        match RedbStorage::open(path) {
            Ok(_) => panic!("released writer opened the migrated file"),
            Err(error) => {
                let message = error.to_string();
                assert!(message.contains("uqa_key_value") && message.contains("u8"), "{message}");
                println!("Released 0.3.6 writer rejected by table types: {message}");
            }
        }
        return;
    }
    let provider = RedbStorage::open(path).unwrap();
    let store = provider.store();
    store.begin_transaction().unwrap();
    store.put(b"", b"").unwrap();
    store.put(b"a\0\xff", b"original\0\xff").unwrap();
    store.savepoint("keep").unwrap();
    store.put(b"discarded", b"discarded").unwrap();
    store.rollback_to_savepoint("keep").unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(store.change_version().unwrap(), Some(1));
}
'''
NEW = r'''
use new_storage::KeyValueStore;
use new_provider::RedbStorage;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let provider = RedbStorage::open(&args[1]).unwrap();
    let store = provider.store();
    assert_eq!(store.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(store.get(b"a\0\xff").unwrap().unwrap(), b"original\0\xff");
    assert_eq!(store.get(b"discarded").unwrap(), None);
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
    target = ROOT / "target" / "redb-legacy-probe"
    with tempfile.TemporaryDirectory(prefix="uqa-redb-legacy-") as temporary:
        project = Path(temporary)
        (project / "src" / "bin").mkdir(parents=True)
        manifest = f'''[package]
name = "uqa-redb-legacy-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
redb = "=4.1.0"
old_provider = {{ package = "uqa-storage-redb", version = "=0.3.6" }}
old_storage = {{ package = "uqa-storage", version = "=0.3.6" }}
new_provider = {{ package = "uqa-storage-redb", path = {json.dumps(str(ROOT / "crates/uqa-storage-redb"))} }}
new_storage = {{ package = "uqa-storage", path = {json.dumps(str(ROOT / "crates/uqa-storage"))} }}
'''
        (project / "Cargo.toml").write_text(manifest)
        (project / "src" / "bin" / "old.rs").write_text(OLD)
        (project / "src" / "bin" / "new.rs").write_text(NEW)
        environment = dict(os.environ, CARGO_BUILD_JOBS="2")
        subprocess.run(
            ["cargo", "build", "--manifest-path", str(project / "Cargo.toml"), "--target-dir", str(target)],
            check=True,
            env=environment,
        )
        checksum = registry_package_checksum(project / "Cargo.toml", "uqa-storage-redb", "0.3.6")
        print(f"Released provider checksum: {checksum}", flush=True)
        extension = ".exe" if os.name == "nt" else ""
        old = str(target / "debug" / f"old{extension}")
        new = str(target / "debug" / f"new{extension}")
        database = str(project / "legacy.redb")
        for command in ([old, "create", database], [new, database, "migrate"], [old, "reject", database], [new, database, "reopen"]):
            subprocess.run(command, check=True)
        print("Actual 0.3.6 create, migration, old-writer rejection and reopen passed.")


if __name__ == "__main__":
    main()
