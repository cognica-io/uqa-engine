#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify SQLite native and Key/Value migration with the released 0.3.6 library."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile

from cargo_package_provenance import registry_package_checksum


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
    if args[2].starts_with("graph-") {
        graph(&args);
        return;
    }
    if args[2].starts_with("native-") {
        native(&args);
        return;
    }
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

fn native(args: &[String]) {
    use old_storage::DocumentStore;
    let connection = open(&args[1], std::path::Path::new(&args[3]));
    if args[2] == "native-create" {
        old_provider::Catalog::open(connection.clone()).unwrap();
        let mut documents = old_provider::SQLiteDocumentStore::new(connection, "public.native");
        documents.put(7, std::collections::BTreeMap::new()).unwrap();
        return;
    }
    let error = old_provider::Catalog::open(connection.clone()).err().expect("released catalog accepted the native MVCC format");
    assert!(error.to_string().contains("49"), "unrelated native open failure: {error}");
    let mut documents = old_provider::SQLiteDocumentStore::new(connection.clone(), "public.native");
    assert!(documents.delete(7).is_err(), "released direct store bypassed native guards");
    assert!(documents.put(8, std::collections::BTreeMap::new()).is_err());
    assert!(connection.with(|connection| {
        connection.execute("DELETE FROM _documents", [])?;
        Ok(())
    }).is_err());
    println!("Released native writer rejected for {}", args[1]);
}

fn graph(args: &[String]) {
    let connection = open(&args[1], std::path::Path::new(&args[3]));
    for suffix in [None, Some("Direct"), Some("fresh")] {
        if suffix == Some("fresh") && args[2] != "graph-reject" { continue; }
        let opened = old_provider::SQLiteGraphStore::open(connection.clone(), suffix);
        if args[2] == "graph-reject" {
            let error = opened.err().expect("released graph writer accepted converted tables");
            let message = error.to_string();
            assert!(message.contains("view") || message.contains("entity_kind") || message.contains("__uqa_mvcc_write_permit"), "unrelated graph open failure: {message}");
            continue;
        }
        let mut graph = opened.unwrap();
        graph.create_graph("g").unwrap();
        for id in [1, 2] {
            let mut vertex = old_core::Vertex::new(id, "item");
            vertex.properties.insert("bytes".into(), old_core::Value::Bytes(vec![1,2]));
            graph.add_vertex(vertex, "g").unwrap();
        }
        graph.add_edge(old_core::Edge::new(1,1,2,"rel"),"g").unwrap();
    }
    println!("Released standalone graph {} completed for {}", args[2], args[1]);
}
'''
NEW = r'''
use new_storage::KeyValueStore;
use new_provider::{ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStore};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args[2].starts_with("graph-") {
        graph(&args);
        return;
    }
    if args[2].starts_with("native-") {
        native(&args);
        return;
    }
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

fn native(args: &[String]) {
    use new_storage::{mvcc::{PreparedRecordCommit, VersionedPersistence}, read_control::StorageReadControl};
    use new_provider::mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner};
    use rusqlite::types::ValueRef;
    let connection = open(&args[1], std::path::Path::new(&args[3]));
    let control = StorageReadControl::with_limit(1 << 24);
    let store = new_provider::SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let owner = connection.with_physical(|connection| {
        Ok(connection.query_row("SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = 'public.native'", [], |row| {
            Ok(NativeRecordOwner::Object {
                identity: row.get::<_, Vec<u8>>(0)?.try_into().unwrap(),
                generation: row.get::<_, Vec<u8>>(1)?.try_into().unwrap(),
            })
        })?)
    }).unwrap();
    let original = NativeRecord::encode(NativeRecordFamily::Documents, owner, &[ValueRef::Text(b"public.native"), ValueRef::Integer(7), ValueRef::Text(b"{}"), ValueRef::Null], &control).unwrap();
    let changed = NativeRecord::encode(NativeRecordFamily::Documents, owner, &[ValueRef::Text(b"public.native"), ValueRef::Integer(7), ValueRef::Text(b"{}"), ValueRef::Integer(123)], &control).unwrap();
    let before = store.snapshot(&control).unwrap();
    let found = before.get(original.key(), &control).unwrap().unwrap();
    if args[2] == "native-migrate" {
        assert_eq!(before.sequence().as_u64(), 1);
        assert_eq!(&***found.value().unwrap(), original.row());
        let prepared = PreparedRecordCommit::new(&[changed.write(Some(before.sequence()))], &control).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        store.commit(id, &prepared, &control).unwrap();
        assert_eq!(&***before.get(original.key(), &control).unwrap().unwrap().value().unwrap(), original.row());
    } else {
        assert_eq!(&***found.value().unwrap(), changed.row());
    }
    assert_eq!(store.snapshot(&control).unwrap().sequence().as_u64(), 2);
    connection.with_physical(|connection| {
        assert_eq!(connection.query_row("SELECT tuple_xmin FROM _documents WHERE table_name='public.native' AND doc_id=7", [], |row| row.get::<_, i64>(0))?, 123);
        assert_eq!(connection.query_row("SELECT count(*) FROM _documents", [], |row| row.get::<_, i64>(0))?, 1);
        Ok(())
    }).unwrap();
}

fn graph(args: &[String]) {
    let connection = open(&args[1], std::path::Path::new(&args[3]));
    connection.bind_native_records(new_storage::mvcc::VersionedSessionOptions::default()).unwrap();
    for suffix in [None, Some("DIRECT"), Some("Fresh")] {
        let mut graph = new_provider::SQLiteGraphStore::open(connection.clone(),suffix).unwrap();
        if suffix == Some("Fresh") && args[2] == "graph-migrate" {
            graph.create_graph("g").unwrap();
            for id in [1, 2] {
                let mut vertex = new_core::Vertex::new(id,"item");
                vertex.properties.insert("bytes".into(),new_core::Value::Bytes(vec![1,2]));
                graph.add_vertex(vertex,"g").unwrap();
            }
            graph.add_edge(new_core::Edge::new(1,1,2,"rel"),"g").unwrap();
        }
        assert_eq!(graph.graph_names().unwrap(),vec!["g"]);
        let mut vertex = graph.get_vertex(1).unwrap().unwrap();
        assert_eq!(vertex.properties["bytes"],new_core::Value::Bytes(vec![1,2]));
        assert_eq!(graph.get_edge(1).unwrap().unwrap().target_id,2);
        if args[2] == "graph-migrate" {
            assert_eq!(vertex.label,"item");
            vertex.label="updated".into();
            graph.add_vertex(vertex,"g").unwrap();
        } else {
            assert_eq!(vertex.label,"updated");
        }
        assert_eq!(graph.vertex_ids_by_label("item","g").unwrap(),vec![2]);
    }
    println!("Current standalone graph {} completed for {}", args[2], args[1]);
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
old_core = {{ package = "uqa-core", version = "=0.3.6" }}
new_provider = {{ package = "uqa-storage-sqlite", path = {json.dumps(str(ROOT / "crates/uqa-storage-sqlite"))} }}
new_storage = {{ package = "uqa-storage", path = {json.dumps(str(ROOT / "crates/uqa-storage"))} }}
new_core = {{ package = "uqa-core", path = {json.dumps(str(ROOT / "crates/uqa-core"))} }}
'''
        (project / "Cargo.toml").write_text(manifest)
        (project / "src" / "bin" / "old.rs").write_text(OLD + OPEN)
        (project / "src" / "bin" / "new.rs").write_text(NEW + OPEN)
        subprocess.run(
            ["cargo", "build", "--manifest-path", str(project / "Cargo.toml"), "--target-dir", str(target)],
            check=True,
            env=dict(os.environ, CARGO_BUILD_JOBS="2"),
        )
        checksum = registry_package_checksum(project / "Cargo.toml", "uqa-storage-sqlite", "0.3.6")
        print(f"Released provider checksum: {checksum}", flush=True)
        extension = ".exe" if os.name == "nt" else ""
        old = str(target / "debug" / f"old{extension}")
        new = str(target / "debug" / f"new{extension}")
        for mode in ("plain", "encrypted", "compressed", "compressed-encrypted"):
            database = str(project / f"{mode}.db")
            for binary, action in ((old, "create"), (new, "migrate"), (old, "reject"), (new, "reopen")):
                subprocess.run([binary, mode, action, database], check=True)
            database = str(project / f"native-{mode}.db")
            for binary, action in ((old, "native-create"), (new, "native-migrate"), (old, "native-reject"), (new, "native-reopen")):
                subprocess.run([binary, mode, action, database], check=True)
            database = str(project / f"graph-{mode}.db")
            for binary, action in ((old, "graph-create"), (new, "graph-migrate"), (old, "graph-reject"), (new, "graph-reopen")):
                subprocess.run([binary, mode, action, database], check=True)
        print("Actual 0.3.6 create, migration, old-writer rejection and reopen passed for native, Key/Value and standalone graphs in four modes.")


if __name__ == "__main__":
    main()
