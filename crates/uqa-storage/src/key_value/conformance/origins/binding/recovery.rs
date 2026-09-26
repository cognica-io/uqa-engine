//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A provider process dies with a real completed graph immediately around durable publication.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use super::{
    maintenance::{index, scores, source},
    restore::{rows, verify_diskann_restored},
    runtime::diskann_runtime_fixture_options,
};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        format::DiskANNGeneration,
        maintenance::{DiskANNChangeStatistics, DiskANNStatisticsRequest},
    },
    key_value::conformance::{expect, expect_eq},
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendError, StorageBackendResult, VectorIndexOpenMode,
};

const READY: &str = "diskann-publication-ready";

fn error(error: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(error.to_string())
}

/// Child-process fixture: rebuild the existing restore corpus and retain all publication resources until the parent kills this process. With `committed`, acknowledge the actual provider commit before announcing readiness; otherwise retain its private publication.
pub fn diskann_rebuild_until_process_loss(
    backend: &dyn PersistentStorageBackend,
    committed: bool,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    let captured = source(backend, &control)?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    backend.begin_transaction()?;
    captured.rebuild(diskann_runtime_fixture_options(2)?, &temporary, &control)?;
    if committed {
        backend.commit_transaction()?;
    }
    println!("{READY}");
    std::io::stdout().flush().map_err(error)?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(error)?;
    Err(error(
        "publication owner must be terminated at its announced boundary",
    ))
}

struct Peer(Child);

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run a provider-configured invocation of its existing test executable, wait for the real publication boundary, and terminate it without running Rust destructors. The timeout only detects a stuck fixture; it is not a performance threshold.
pub fn kill_diskann_publication_owner(command: &mut Command) -> StorageBackendResult<()> {
    let mut peer = Peer(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(error)?,
    );
    let output = peer.0.stdout.take().expect("piped output");
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut ready = Err("publication process exited before readiness".into());
        for line in BufReader::new(output).lines() {
            match line {
                Ok(line) if line.trim() == READY => {
                    ready = Ok(());
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    ready = Err(error.to_string());
                    break;
                }
            }
        }
        let _ = send.send(ready);
    });
    let ready = receive.recv_timeout(Duration::from_secs(60));
    let killed = peer.0.kill();
    let status = peer.0.wait();
    let joined = reader.join();
    ready.map_err(error)?.map_err(error)?;
    killed.map_err(error)?;
    expect(joined.is_ok(), "publication output reader completed")?;
    expect(
        !status.map_err(error)?.success(),
        "publication owner was actually terminated",
    )
}

/// Verify an actual cold recovery of the restore corpus, including canonical rows and tensors, selected physical ownership and exact uncovered changes. A confirmed rebuild changes only the generation and its coverage.
pub fn verify_diskann_recovered_publication(
    backend: &dyn PersistentStorageBackend,
    previous: DiskANNGeneration,
    committed: bool,
) -> StorageBackendResult<DiskANNGeneration> {
    if !committed {
        verify_diskann_restored(backend, previous)?;
        return Ok(previous);
    }
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    let statistics = source(backend, &control)?.statistics(
        DiskANNStatisticsRequest {
            after: None,
            max_records: 64,
        },
        &control,
    )?;
    let current = statistics.generation;
    expect_eq(
        &(current.database(), current.table(), current.index()),
        &(previous.database(), previous.table(), previous.index()),
        "recovered publication retains physical ownership",
    )?;
    expect(
        current.generation() > previous.generation(),
        "durable publication selects a strictly newer generation",
    )?;
    expect_eq(
        &statistics.outstanding,
        &DiskANNChangeStatistics::default(),
        "recovered rebuild covers all previously committed changes",
    )?;
    expect(statistics.next.is_none(), "complete recovery census")?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let live = index(backend, &temporary, &control, VectorIndexOpenMode::Restore)?;
    expect_eq(&live.count()?, &5, "recovered complete tensor cardinality")?;
    scores(&*live, &[(1, -1.0), (3, -1.0), (4, 0.0), (5, 1.0)])?;
    rows(backend, false)?;
    Ok(current)
}
