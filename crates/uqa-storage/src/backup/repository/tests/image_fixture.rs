//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Unit {
    pub namespace: u8,
    pub generation: u64,
    pub key: u64,
    pub kind: UnitKind,
    pub data: Vec<u8>,
}

impl Unit {
    pub fn record(key: u64, data: &[u8]) -> Self {
        Self {
            namespace: 7,
            generation: 0,
            key,
            kind: UnitKind::Record,
            data: data.to_vec(),
        }
    }
    fn order(&self) -> (u8, u64, u64) {
        (self.namespace, self.generation, self.key)
    }
}

pub(super) struct Image {
    cut: BackupCut,
    units: Vec<Unit>,
    next: usize,
    current: Option<usize>,
    key: [u8; 8],
    offset: usize,
    statistics: BackupSourceStatistics,
    pub dropped: Arc<AtomicBool>,
    pub length_delta: u64,
    pub cancel: Option<CancellationToken>,
}

impl Image {
    pub fn new(mut units: Vec<Unit>) -> Self {
        units.sort_by_key(Unit::order);
        Self {
            cut: cut(),
            units,
            next: 0,
            current: None,
            key: [0; 8],
            offset: 0,
            statistics: BackupSourceStatistics::default(),
            dropped: Arc::new(AtomicBool::new(false)),
            length_delta: 0,
            cancel: None,
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl BackupImageSource for Image {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        assert!(maximum > 0);
        self.statistics.visited_positions += 1;
        if self.next == self.units.len() {
            self.current = None;
            return Ok(BackupSourceStep::Complete);
        }
        self.current = Some(self.next);
        self.key = self.units[self.next].key.to_be_bytes();
        self.next += 1;
        self.offset = 0;
        Ok(BackupSourceStep::Unit)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        let value = &self.units[self.current?];
        Some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes([value.namespace; 16]).unwrap(),
                generation: value.generation,
                family: 1,
                key: &self.key,
            },
            kind: value.kind,
            decoded_bytes: value.data.len() as u64 + self.length_delta,
        })
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        assert!(output.len() <= 4096);
        let data = &self.units[self.current.unwrap()].data;
        let count = output.len().min(data.len() - self.offset);
        output[..count].copy_from_slice(&data[self.offset..self.offset + count]);
        self.offset += count;
        self.statistics.copied_payload_bytes += count as u64;
        if let Some(token) = &self.cancel {
            token.cancel();
        }
        Ok(count)
    }
}

pub(super) fn capture(
    repository: &LocalRepository,
    parent: Option<BackupId>,
    units: Vec<Unit>,
    control: &StorageReadControl,
    workspace: u64,
) -> (BackupRoot, BackupCaptureReport) {
    let source = Image::new(units);
    let dropped = source.dropped.clone();
    let mut operation = repository
        .begin_backup(source.cut(), parent, 64 << 20, control)
        .unwrap();
    let report = operation.prepare_image(source, workspace, control).unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    (operation.publish(control).unwrap(), report)
}

pub(super) fn read(
    repository: &LocalRepository,
    root: BackupRoot,
    control: &StorageReadControl,
) -> Vec<(Unit, ObjectSpan)> {
    let lease = repository.open_backup(root.backup(), control).unwrap();
    let mut input = lease.read_image(16 << 20, control).unwrap();
    let mut result = Vec::new();
    loop {
        match input.advance(4096).unwrap() {
            BackupReadStep::Complete => break,
            BackupReadStep::Pending => continue,
            BackupReadStep::Unit => (),
        }
        let entry = input.current_unit().unwrap();
        let mut unit = Unit {
            namespace: entry.address().namespace.as_bytes()[0],
            generation: entry.address().generation,
            key: u64::from_be_bytes(entry.address().key.try_into().unwrap()),
            kind: entry.kind(),
            data: Vec::new(),
        };
        let span = entry.object();
        let mut buffer = [0; 4096];
        loop {
            let n = input.read_current(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            unit.data.extend_from_slice(&buffer[..n]);
        }
        result.push((unit, span));
    }
    assert_eq!(input.verified().unwrap().root(), root);
    result
}

pub(super) fn objects(repository: &LocalRepository) -> (usize, u64) {
    let files = repository
        .objects
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .collect::<Vec<_>>();
    (files.len(), files.iter().sum())
}
