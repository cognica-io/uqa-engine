//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Clone, Copy)]
enum Fault {
    Reverse,
    Missing,
    Duplicate,
    Extra,
    Short,
    Corrupt,
    Foreign,
    Suppressed,
    NoRecord,
    RepeatedRecord,
    MaterializedRecord,
    Codes,
    Side,
}

struct Faulty {
    source: DiskANNMemorySource,
    fault: Fault,
}

fn reseal_record(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..64]);
    hash.update(&bytes[96..]);
    bytes[64..96].copy_from_slice(&hash.finalize());
}

fn reseal_page(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..112]);
    hash.update(&bytes[144..]);
    bytes[112..144].copy_from_slice(&hash.finalize());
}

impl DiskANNPageSource for Faulty {
    fn generation(&self) -> DiskANNGeneration {
        self.source.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.source.capabilities()
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if matches!(self.fault, Fault::NoRecord) {
            return Ok(());
        }
        self.source.read_record(key, limit, control, &mut |bytes| {
            if matches!(self.fault, Fault::MaterializedRecord) {
                let owned = copy(bytes, limit, control)?;
                return visit(&owned);
            }
            if matches!(self.fault, Fault::RepeatedRecord) {
                let _ = visit(bytes);
                let _ = visit(bytes);
                return Ok(());
            }
            if matches!(
                (self.fault, key),
                (Fault::Codes, DiskANNRecordKey::Codes(0))
                    | (Fault::Side, DiskANNRecordKey::Side(0))
            ) {
                let mut bytes = bytes.to_vec();
                match self.fault {
                    Fault::Codes => bytes[168] ^= 1,
                    Fault::Side => bytes[128..136].copy_from_slice(&2000_u64.to_le_bytes()),
                    _ => unreachable!(),
                }
                reseal_record(&mut bytes);
                visit(&bytes)
            } else {
                visit(bytes)
            }
        })
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if matches!(self.fault, Fault::Missing) {
            return Ok(());
        }
        let mut pages = pages.to_vec();
        if matches!(self.fault, Fault::Reverse) {
            pages.reverse();
        }
        self.source
            .read_graph_pages(&pages, control, &mut |id, bytes| match self.fault {
                Fault::Duplicate => {
                    visit(id, bytes)?;
                    visit(id, bytes)
                }
                Fault::Extra => visit(u64::MAX, bytes),
                Fault::Short => visit(id, &bytes[..bytes.len() - 1]),
                Fault::Corrupt => {
                    let mut bytes = bytes.to_vec();
                    bytes[150] ^= 1;
                    visit(id, &bytes)
                }
                Fault::Foreign => {
                    let mut bytes = bytes.to_vec();
                    bytes[16] ^= 1;
                    reseal_page(&mut bytes);
                    visit(id, &bytes)
                }
                Fault::Suppressed => {
                    let _ = visit(id, &bytes[..1]);
                    Ok(())
                }
                _ => visit(id, bytes),
            })
    }
}

#[test]
fn materialized_record_and_reader_copy_share_the_parent_without_halving_the_record_limit() {
    let fixture = fixture(8, 1, 0);
    let physical = MemoryBudget::new(65_536);
    let work = StorageReadControl::with_limit(65_536);
    let source = Faulty {
        source: fixture.memory(&physical, &work).unwrap(),
        fault: Fault::MaterializedRecord,
    };
    let size = fixture.manifest.encode(&work).unwrap().len();
    let control = StorageReadControl::with_limit(size * 2);
    let bytes = read_record(&source, DiskANNRecordKey::Manifest, size, &control).unwrap();
    assert_eq!(bytes.len(), size);
    assert_eq!(control.memory().used(), size);
    assert_eq!(control.memory().peak(), size * 2);
    drop(bytes);
    assert_eq!(control.memory().used(), 0);
    for (record, parent) in [(size - 1, size * 2), (size, size * 2 - 1)] {
        let control = StorageReadControl::with_limit(parent);
        assert!(matches!(
            read_record(&source, DiskANNRecordKey::Manifest, record, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn reordered_batches_preserve_request_order_and_bad_completions_fail_closed() {
    let fixture = fixture(512, 3, 0);
    let physical = MemoryBudget::new(65_536);
    let work = StorageReadControl::with_limit(65_536);
    let source = fixture.memory(&physical, &work).unwrap();
    let query = StorageReadControl::with_limit(65_536);
    for fault in [
        Fault::Reverse,
        Fault::Missing,
        Fault::Duplicate,
        Fault::Extra,
        Fault::Short,
        Fault::Corrupt,
        Fault::Foreign,
        Fault::Suppressed,
    ] {
        let reader = fixture
            .reader(
                Arc::new(Faulty {
                    source: source.clone(),
                    fault,
                }),
                limits(0),
                &work,
            )
            .unwrap();
        let result = reader.read_pages(&[0, 1, 2], &query);
        if matches!(fault, Fault::Reverse) {
            let pages = result.unwrap();
            assert_eq!(
                pages.iter().map(DiskANNPageLease::id).collect::<Vec<_>>(),
                [0, 1, 2]
            );
            for (id, page) in pages.iter().enumerate() {
                assert_eq!(page.bytes(), fixture.graph[id]);
            }
        } else {
            assert!(result.is_err());
        }
        assert_eq!(query.memory().used(), 0);
        let result = reader.read_nodes(&[2, 0, 1], &query);
        if matches!(fault, Fault::Reverse) {
            assert_eq!(
                result
                    .unwrap()
                    .iter()
                    .map(DiskANNNode::node_id)
                    .collect::<Vec<_>>(),
                [2, 0, 1]
            );
        } else {
            assert!(result.is_err());
        }
        assert_eq!(query.memory().used(), 0);
        assert!(reader.read_pages(&[1, 0], &query).is_err());
        assert!(reader.read_pages(&[0, 0], &query).is_err());
        assert!(reader.read_pages(&[3], &query).is_err());
    }
}

#[test]
fn metadata_callback_failures_catalog_mismatches_and_stream_digest_changes_are_rejected() {
    let fixture = fixture(8, 4, 1);
    let physical = MemoryBudget::new(65_536);
    let work = StorageReadControl::with_limit(65_536);
    let source = fixture.memory(&physical, &work).unwrap();
    for fault in [Fault::NoRecord, Fault::RepeatedRecord, Fault::Codes] {
        assert!(fixture
            .reader(
                Arc::new(Faulty {
                    source: source.clone(),
                    fault
                }),
                limits(0),
                &work
            )
            .is_err());
        assert_eq!(work.memory().used(), 0);
    }
    let reader = fixture
        .reader(
            Arc::new(Faulty {
                source: source.clone(),
                fault: Fault::Side,
            }),
            limits(0),
            &work,
        )
        .unwrap();
    let query = StorageReadControl::with_limit(65_536);
    assert!(reader.visit_side(&query, &mut |_| Ok(())).is_err());
    assert_eq!(query.memory().used(), 0);
    drop(reader);
    let mut params = fixture.manifest.input().parameters;
    params.seed += 1;
    assert!(DiskANNReader::open(Arc::new(source), 8, params, limits(0), &work).is_err());
    assert_eq!(work.memory().used(), 0);
}

#[test]
fn sealing_checks_node_and_side_order_across_page_and_record_boundaries() {
    let fixture = fixture(512, 2, 2);
    let control = StorageReadControl::with_limit(65_536);
    let mut seal = fixture.sealer(&control);
    seal.graph_page(0, &fixture.graph[0]).unwrap();
    let mut wrong_node = fixture.graph[1].clone();
    wrong_node[PAGE_HEADER_BYTES + 8..PAGE_HEADER_BYTES + 16].copy_from_slice(&9_u64.to_le_bytes());
    reseal_page(&mut wrong_node);
    assert!(seal.graph_page(1, &wrong_node).is_err());
    assert!(seal.finish().is_err());
    let mut seal = DiskANNArtifactSealer::new(fixture.manifest, &control).unwrap();
    for (key, bytes) in &fixture.records {
        if *key == DiskANNRecordKey::Side(0) {
            seal.side_batch(0, bytes).unwrap();
        }
        if *key == DiskANNRecordKey::Side(1) {
            let mut repeated = bytes.clone();
            repeated[128..136].copy_from_slice(&1000_u64.to_le_bytes());
            reseal_record(&mut repeated);
            assert!(seal.side_batch(1, &repeated).is_err());
        }
    }
    assert!(seal.finish().is_err());
    let mut input = *fixture.manifest.input();
    input.artifacts.graph[0] ^= 1;
    let mut changed = fixture;
    changed.manifest = DiskANNManifest::new(input).unwrap();
    let mut seal = changed.sealer(&control);
    for (id, page) in changed.graph.iter().enumerate() {
        seal.graph_page(id as u64, page).unwrap();
    }
    assert!(seal.finish().is_err());
    assert_eq!(control.memory().used(), 0);
}
