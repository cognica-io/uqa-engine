//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::sequence::registry::SequenceCatalogInput;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;
use uqa_sql::SQLError;

const FULL: ImageDecodeWindow = ImageDecodeWindow {
    bytes: usize::MAX,
    steps: usize::MAX,
};

struct Counted<'a> {
    input: Cursor<&'a [u8]>,
    calls: usize,
}
impl Read for Counted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        self.input.read(output)
    }
}
struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("unexpected sequence image read")
    }
}

fn multiple() -> SequenceReadSnapshot {
    let mut source = fixture();
    let catalog = uqa_core::memory::RetainedValue::make_mut(&mut source.catalog);
    let record = *catalog.values().next().unwrap();
    for value in 0_u8..3 {
        catalog
            .insert(
                RelationIdentity::new("s", format!("z{value}{}", "한🙂".repeat(1025))),
                SequenceRecord {
                    object_id: [30 + value; 16],
                    catalog_oid: Some(31_100 + u32::from(value)),
                    state: record,
                    persistence: RelationPersistence::Permanent,
                },
                BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
            )
            .unwrap();
    }
    source
}

#[test]
fn sequence_input_bounds_both_images_and_retains_native_mutation_owners() {
    let source = multiple();
    let (metadata, authority) = encode(&source);
    for (width, steps) in [(1, 7), (17, 1), (4096, 3)] {
        let memory = MemoryBudget::new(1 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = SequenceCatalogInput::new(&memory, &mutations, &token).unwrap();
        let window = ImageDecodeWindow {
            bytes: width,
            steps,
        };
        let mut reader = Counted {
            input: Cursor::new(metadata.as_slice()),
            calls: 0,
        };
        loop {
            let position = reader.input.position();
            let calls = reader.calls;
            let complete = input.advance_metadata(&mut reader, window).unwrap();
            assert!(reader.input.position() - position <= width as u64);
            assert!(reader.calls - calls <= steps);
            if complete {
                break;
            }
        }
        assert_eq!(reader.input.position(), metadata.len() as u64);
        let mut reader = Counted {
            input: Cursor::new(authority.as_slice()),
            calls: 0,
        };
        let mut restored = loop {
            let position = reader.input.position();
            let calls = reader.calls;
            let value = input.advance_authority(&mut reader, window).unwrap();
            assert!(reader.input.position() - position <= width as u64);
            assert!(reader.calls - calls <= steps);
            if let Some(value) = value {
                break value;
            }
        };
        assert_eq!(reader.input.position(), authority.len() as u64);
        assert_same(&source, &restored);
        assert!(input.advance_authority(&mut NoRead, FULL).is_err());
        assert!(input.advance_metadata(&mut NoRead, FULL).is_err());
        let retained = restored.clone();
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        restored.remove(&RelationIdentity::new("s", "q")).unwrap();
        assert_eq!(retained.len(), restored.len() + 1);
        drop((restored, retained, full));
        assert_eq!(memory.used(), 0);
        assert_eq!(mutations.used(), 0);
    }
}

fn advance(
    input: &mut SequenceCatalogInput,
    reader: &mut dyn Read,
    window: ImageDecodeWindow,
    authority: bool,
) -> Result<bool, SQLError> {
    if authority {
        input
            .advance_authority(reader, window)
            .map(|value| value.is_some())
    } else {
        input.advance_metadata(reader, window)
    }
}

fn retire(authority: bool, failure: &str) {
    let (metadata, authority_bytes) = encode(&fixture());
    let memory = MemoryBudget::new(65536);
    let mutations = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let mut input = SequenceCatalogInput::new(&memory, &mutations, &token).unwrap();
    if authority {
        assert!(input
            .advance_metadata(&mut metadata.as_slice(), FULL)
            .unwrap());
    }
    let mut bytes = if authority { authority_bytes } else { metadata };
    if failure == "short" {
        bytes.pop();
    } else if failure == "trailing" {
        bytes.push(0);
    }
    let mut reader = Cursor::new(bytes);
    assert!(!advance(
        &mut input,
        &mut reader,
        ImageDecodeWindow {
            bytes: 1,
            steps: 64
        },
        authority
    )
    .unwrap());
    assert_eq!(reader.position(), 1);
    assert!(memory.used() > 0);
    if failure == "drop" {
        drop(input);
        assert_eq!(memory.used(), 0);
        return;
    }
    let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
    if failure == "cancel" {
        token.cancel();
    }
    let window = ImageDecodeWindow {
        bytes: if failure == "bytes" { 0 } else { 17 },
        steps: if failure == "steps" { 0 } else { usize::MAX },
    };
    if failure == "order" {
        assert!(advance(&mut input, &mut NoRead, FULL, !authority).is_err());
    } else if failure == "unwind" {
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = advance(&mut input, &mut NoRead, window, authority);
        }))
        .is_err());
    } else {
        let mut rejected = false;
        for _ in 0..1024 {
            match advance(&mut input, &mut reader, window, authority) {
                Err(_) => {
                    rejected = true;
                    break;
                }
                Ok(false) => (),
                Ok(true) => panic!("{failure} completed invalid sequence input"),
            }
        }
        assert!(rejected, "{failure} did not retire sequence input");
    }
    drop(full);
    assert_eq!(memory.used(), 0, "{failure}");
    assert_eq!(mutations.used(), 0, "{failure}");
    assert!(input.advance_metadata(&mut NoRead, FULL).is_err());
    assert!(input.advance_authority(&mut NoRead, FULL).is_err());
}

#[test]
fn sequence_input_retires_both_phases_on_failure_unwind_and_wrong_order() {
    for authority in [false, true] {
        for failure in [
            "bytes", "steps", "cancel", "memory", "short", "trailing", "unwind", "drop", "order",
        ] {
            retire(authority, failure);
        }
    }
}

#[test]
fn sequence_membership_validation_yields_without_source_reads_and_retires_on_cancel() {
    let source = multiple();
    let (metadata, authority) = encode(&source);
    for cancel in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = SequenceCatalogInput::new(&memory, &memory, &token).unwrap();
        assert!(input
            .advance_metadata(&mut metadata.as_slice(), FULL)
            .unwrap());
        let mut reader = Cursor::new(authority.as_slice());
        assert!(input
            .advance_authority(&mut reader, FULL)
            .unwrap()
            .is_none());
        assert_eq!(reader.position(), authority.len() as u64);
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        for _ in 0..source.catalog.len() {
            assert!(input
                .advance_authority(&mut NoRead, window)
                .unwrap()
                .is_none());
        }
        if cancel {
            token.cancel();
            assert!(input.advance_authority(&mut NoRead, window).is_err());
        } else {
            let restored = input
                .advance_authority(&mut NoRead, window)
                .unwrap()
                .unwrap();
            assert_same(&source, &restored);
            drop(restored);
        }
        assert_eq!(memory.used(), 0);
    }
}
