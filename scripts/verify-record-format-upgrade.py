#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify development-format upgrades with an actual previous Git revision's binaries."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import tomllib


ROOT = Path(__file__).resolve().parents[1]
DRIVER = r'''
use std::{io::{BufRead, Write}, path::Path, sync::Arc};
use uqa_storage::mvcc::{CommitStatus, IdentifierRequest, PreparedRecordCommit, RecordWrite, StorageTransactionId, VersionedPersistence, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStore, SQLiteRecordStore};

fn control() -> StorageReadControl { StorageReadControl::with_limit(16 << 20) }

fn open(kind: &str, mode: &str, path: &Path, create: bool) -> Result<Arc<dyn VersionedPersistence>, String> {
    if kind == "redb" {
        let provider = uqa_storage_redb::RedbStorage::open(path).map_err(|e| e.to_string())?;
        return provider.record_store().map(|store| Arc::new(store) as Arc<dyn VersionedPersistence>).map_err(|e| e.to_string());
    }
    let connection = match mode {
        "plain" => ManagedConnection::open(path),
        "encrypted" => ManagedConnection::open_encrypted(path, "format fixture key"),
        "compressed" => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        "compressed-encrypted" => ManagedConnection::open_compressed_encrypted(path, "format fixture key", SQLiteCompressionOptions::default()),
        _ => panic!("unknown mode"),
    }.map_err(|e| e.to_string())?;
    let records = if kind == "native" {
        if create {
            let catalog = Catalog::open(connection.clone()).map_err(|e| e.to_string())?;
            connection.bind_native_records(VersionedSessionOptions::default()).map_err(|e| e.to_string())?;
            catalog.set_metadata("format_probe", "original").map_err(|e| e.to_string())?;
        }
        SQLiteRecordStore::for_native(&connection, &control())
    } else {
        SQLiteKeyValueStore::new(connection.clone()).map_err(|e| e.to_string())?;
        SQLiteRecordStore::new(&connection)
    }.map_err(|e| e.to_string())?;
    Ok(Arc::new(records))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn report(records: &dyn VersionedPersistence, committed: u64, pending: u64) -> serde_json::Value {
    let control = control();
    let snapshot = records.snapshot(&control).unwrap();
    let page = snapshot.scan(b"", None, 1024, &control).unwrap();
    assert!(page.len() < 1024, "the fixture report must include every record");
    let rows = page.iter().map(|row| serde_json::json!({
        "key": hex(&row.key),
        "revision": row.version.sequence().as_u64(),
        "value": row.version.value().map(|value| hex(value)),
    })).collect::<Vec<_>>();
    let committed_id = StorageTransactionId::new(records.database_id(), committed).unwrap();
    let CommitStatus::Committed(receipt) = records.commit_status(committed_id, &control).unwrap() else {
        panic!("committed receipt disappeared");
    };
    let pending_id = StorageTransactionId::new(records.database_id(), pending).unwrap();
    assert_eq!(records.commit_status(pending_id, &control).unwrap(), CommitStatus::Pending);
    serde_json::json!({
        "database": hex(&records.database_id().as_bytes()),
        "sequence": snapshot.sequence().as_u64(),
        "committed": committed,
        "pending": pending,
        "receipt_sequence": receipt.sequence.as_u64(),
        "receipt_fingerprint": hex(&receipt.fingerprint),
        "identifier_watermark": records.identifier_watermark(b"format-probe", &control).unwrap(),
        "rows": rows,
    })
}

fn rejected<T, E: std::fmt::Display>(result: Result<T, E>, native_reopen: bool) {
    let error = result.err().expect("previous writer accepted the new record format").to_string();
    // Native reopen validates physical families and exact cache triggers before the common record version. Retained handles must still reject through their record-format guard.
    let native_schema_fence = native_reopen && matches!(error.as_str(),
        "invalid versioned record encoding: unmapped native table requires an explicit record family"
        | "invalid versioned record encoding: missing or changed native materialization schema or guard"
        | "SQLite storage failed: storage backend error: missing or changed metadata cache trigger");
    assert!(error.contains("record format") || error.contains("record table definition") || native_schema_fence, "unrelated failure: {error}");
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let (kind, mode, action) = (&args[1], &args[2], args[3].as_str());
    let path = Path::new(&args[4]);
    let state = Path::new(&args[5]);
    let opened = open(kind, mode, path, action == "create");
    if action == "reject" {
        rejected(opened, kind == "native");
        return;
    }
    let records = opened.unwrap();
    let control = control();
    let empty = PreparedRecordCommit::new(&[], &control).unwrap();
    if action == "create" {
        let id = records.allocate_transaction(&control).unwrap();
        let writes = if kind == "native" { empty } else {
            PreparedRecordCommit::new(&[RecordWrite { key: b"fixture\0\xff", expected: None, value: Some(b"original\0\xff") }], &control).unwrap()
        };
        records.commit(id, &writes, &control).unwrap();
        let pending = records.allocate_transaction(&control).unwrap();
        records.allocate_identifiers(b"format-probe", IdentifierRequest::Observe(77), &control).unwrap();
        std::fs::write(state, serde_json::to_vec(&report(&*records, id.allocation(), pending.allocation())).unwrap()).unwrap();
        return;
    }
    let expected: serde_json::Value = serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
    let committed = expected["committed"].as_u64().unwrap();
    let pending = expected["pending"].as_u64().unwrap();
    assert_eq!(report(&*records, committed, pending), expected);
    if action == "hold" {
        assert_ne!(kind, "redb", "redb retains its exclusive file owner");
        let snapshot = records.snapshot(&control).unwrap();
        let pending_id = StorageTransactionId::new(records.database_id(), pending).unwrap();
        println!("ready");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "upgraded");
        rejected(records.snapshot(&control), false);
        rejected(snapshot.get(b"fixture\0\xff", &control), false);
        rejected(records.allocate_transaction(&control), false);
        rejected(records.identifier_watermark(b"format-probe", &control), false);
        rejected(records.allocate_identifiers(b"format-probe", IdentifierRequest::Observe(78), &control), false);
        rejected(records.commit(pending_id, &empty, &control), false);
        rejected(records.abort(pending_id, &control), false);
        rejected(records.reclaim_versions(&control), false);
        return;
    }
    assert_eq!(action, "verify");
    let next = records.allocate_transaction(&control).unwrap();
    assert!(next.allocation() > pending, "upgrade reused a predecessor allocation");
    assert_eq!(records.abort(next, &control).unwrap(), CommitStatus::Aborted);
    assert_eq!(report(&*records, committed, pending), expected);
}
'''


def build(source: Path, project: Path, label: str, target: Path, offline: bool) -> Path:
    (project / "src").mkdir(parents=True)
    packages = tomllib.loads((source / "Cargo.lock").read_text())["package"]
    serde_version = next(package["version"] for package in packages if package["name"] == "serde_json")
    name = f"uqa-record-format-{label}"
    manifest = f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n[dependencies]\nserde_json = "={serde_version}"\n'
    for crate in ("uqa-storage", "uqa-storage-sqlite", "uqa-storage-redb"):
        manifest += f'{crate} = {{ path = {json.dumps(str(source / "crates" / crate))} }}\n'
    (project / "Cargo.toml").write_text(manifest)
    (project / "src/main.rs").write_text(DRIVER)
    shutil.copyfile(source / "Cargo.lock", project / "Cargo.lock")
    network = ["--offline"] if offline else []
    subprocess.run(
        ["cargo", "metadata", *network, "--format-version", "1", "--manifest-path", str(project / "Cargo.toml")],
        stdout=subprocess.PIPE,
        check=True,
    )
    expected = {(package["name"], package["version"], package.get("source"), package.get("checksum")) for package in packages if package.get("source")}
    resolved = tomllib.loads((project / "Cargo.lock").read_text())["package"]
    unexpected = {(package["name"], package["version"], package.get("source"), package.get("checksum")) for package in resolved if package.get("source")} - expected
    if unexpected:
        raise RuntimeError(f"probe dependencies differ from the tested source lockfile: {sorted(unexpected)}")
    subprocess.run(
        ["cargo", "build", *network, "--locked", "--manifest-path", str(project / "Cargo.toml"), "--target-dir", str(target)],
        check=True,
    )
    return target / "debug" / (name + (".exe" if os.name == "nt" else ""))


def verify(previous: Path, current: Path, directory: Path, kind: str, mode: str) -> None:
    database = directory / f"{kind}-{mode}.db"
    state = database.with_suffix(".json")
    def command(binary: Path, action: str) -> list[str]:
        return [str(binary), kind, mode, action, str(database), str(state)]
    subprocess.run(command(previous, "create"), check=True)
    if kind == "redb":
        subprocess.run(command(current, "verify"), check=True)
    else:
        retained = subprocess.Popen(command(previous, "hold"), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            assert retained.stdout is not None
            ready: queue.Queue[str] = queue.Queue()
            reader = threading.Thread(target=lambda: ready.put(retained.stdout.readline()), daemon=True)
            reader.start()
            assert ready.get(timeout=60).strip() == "ready"
            reader.join()
            subprocess.run(command(current, "verify"), check=True)
            assert retained.stdin is not None
            retained.stdin.write("upgraded\n")
            retained.stdin.flush()
            assert retained.wait(timeout=60) == 0
        finally:
            if retained.poll() is None:
                retained.kill()
                retained.wait()
    subprocess.run(command(previous, "reject"), check=True)
    subprocess.run(command(current, "verify"), check=True)
    print(f"Verified {kind}/{mode}: previous writer rejected; original records and receipts preserved", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-ref", required=True, help="Git revision of the previous development record format")
    parser.add_argument("--offline", action="store_true", help="Use only locally cached dependencies")
    parser.add_argument("--target-dir", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")), help="Reuse this Cargo target directory (default: CARGO_TARGET_DIR or workspace target)")
    parser.add_argument("--provider", action="append", choices=("native", "key-value", "redb"), help="Verify only the named provider; repeat for multiple providers (default: all)")
    args = parser.parse_args()
    revision = subprocess.check_output(["git", "rev-parse", "--verify", "--end-of-options", args.old_ref + "^{commit}"], cwd=ROOT, text=True).strip()
    target = args.target_dir.resolve()
    with tempfile.TemporaryDirectory(prefix="uqa-record-format-") as temporary:
        directory = Path(temporary)
        source = directory / "previous-source"
        source.mkdir()
        archive = subprocess.Popen(["git", "archive", revision], cwd=ROOT, stdout=subprocess.PIPE)
        assert archive.stdout is not None
        try:
            subprocess.run(["tar", "-x", "-C", str(source)], stdin=archive.stdout, check=True)
        finally:
            archive.stdout.close()
        assert archive.wait() == 0
        previous = build(source, directory / "previous-probe", "previous", target, args.offline)
        current = build(ROOT, directory / "current-probe", "current", target, args.offline)
        print(f"Previous source revision: {revision}", flush=True)
        count = 0
        for kind in dict.fromkeys(args.provider or ("native", "key-value", "redb")):
            modes = ("plain",) if kind == "redb" else ("plain", "encrypted", "compressed", "compressed-encrypted")
            for mode in modes:
                verify(previous, current, directory, kind, mode)
                count += 1
        print(f"Development record-format upgrade verified in {count} provider/file configurations.", flush=True)


if __name__ == "__main__":
    main()
