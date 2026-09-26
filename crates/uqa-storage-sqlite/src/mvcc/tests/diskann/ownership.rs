//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native liveness survives independent adapters and is released by actual process death.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
};
use uqa_storage::{
    diskann_index::{format::DiskANNGeneration, pages::DiskANNRecordKey},
    key_value::KeyValueDiskANNStore,
    read_control::StorageReadControl,
};

use super::*;

const PATH_ENV: &str = "UQA_DISKANN_BUILD_OWNER_PATH";
const MODE_ENV: &str = "UQA_DISKANN_BUILD_OWNER_MODE";
const NATIVE_ENV: &str = "UQA_DISKANN_BUILD_OWNER_NATIVE";

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn store(path: &Path, mode: u8, native: bool) -> Arc<dyn KeyValueStore> {
    let connection = connection(path, mode);
    if native {
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        crate::Catalog::open(connection.clone()).unwrap();
        Arc::new(connection.native_diskann_records().unwrap())
    } else {
        Arc::new(SQLiteKeyValueStore::new(connection).unwrap())
    }
}

#[test]
fn diskann_process_death_releases_the_actual_build_owner_in_all_file_modes() {
    let control = StorageReadControl::with_limit(1 << 20);
    if let Some(path) = std::env::var_os(PATH_ENV) {
        let store = store(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
            std::env::var(NATIVE_ENV).unwrap() == "true",
        );
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_stage(31, 32, &control).unwrap();
        stage.start(&control).unwrap();
        stage
            .write_record(DiskANNRecordKey::Codes(0), b"retained", 64, &control)
            .unwrap();
        println!("diskann-owner-ready:{}", stage.generation().generation());
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        panic!("retained child must be terminated by its parent");
    }
    for (mode, native) in (0..4).flat_map(|mode| [false, true].map(move |native| (mode, native))) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.db");
        let (_, test) = concat!(
            module_path!(),
            "::diskann_process_death_releases_the_actual_build_owner_in_all_file_modes"
        )
        .split_once("::")
        .unwrap();
        let mut peer = Peer(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(PATH_ENV, &path)
                .env(MODE_ENV, mode.to_string())
                .env(NATIVE_ENV, native.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(peer.0.stdout.take().unwrap());
        let allocation = loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "child exited before retaining its build"
            );
            if let Some(allocation) = line.trim().strip_prefix("diskann-owner-ready:") {
                break allocation.parse().unwrap();
            }
        };
        let store = store(&path, mode, native);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let generation = DiskANNGeneration::new(
            repository.data_identity(&control).unwrap(),
            31,
            32,
            allocation,
        )
        .unwrap();
        assert!(!repository
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
        assert!(repository.resume_stage(generation, &control).is_err());
        peer.0.kill().unwrap();
        assert!(!peer.0.wait().unwrap().success());
        assert!(!repository
            .reclaim_abandoned_step(generation, 1, &control)
            .unwrap());
        drop((repository, store));
        let reopened = self::store(&path, mode, native);
        let repository = KeyValueDiskANNStore::connect(&reopened, &control).unwrap();
        assert!(repository
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
        assert!(repository.resume_stage(generation, &control).is_err());
    }
}
