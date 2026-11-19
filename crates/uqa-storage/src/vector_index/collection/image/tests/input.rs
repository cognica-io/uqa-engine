//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::build::DiskANNTemporaryBudget;
use std::{io::Read, sync::Arc};
use uqa_core::image_io::ImageDecodeWindow;

struct Wire<'a> {
    bytes: std::io::Cursor<&'a [u8]>,
    end_reads: usize,
}

impl Read for Wire<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert_eq!(self.end_reads, 0, "native registration reread EOF");
        let count = self.bytes.read(output)?;
        self.end_reads += usize::from(count == 0);
        Ok(count)
    }
}

const WINDOW: ImageDecodeWindow = ImageDecodeWindow {
    bytes: 17,
    steps: 1,
};

fn input(
    registry: &MemoryCaptureRegistry,
    control: &StorageReadControl,
    name: Arc<str>,
) -> ImageDecoder<MemoryVectorIndexesBuilder, StorageBackendError> {
    MemoryVectorIndexesBuilder::new(registry, control)
        .add_index(field("prior", control), decoded(control))
        .unwrap()
        .image_input(name, &DiskANNTemporaryBudget::new(0))
        .unwrap()
}

fn fixture() -> (Arc<str>, Vec<u8>) {
    let name = Arc::from("long_한글_".repeat(2048));
    let control = StorageReadControl::with_limit(1 << 20);
    let bytes = encode(&*decoded(&control));
    assert_eq!(control.memory().used(), 0);
    (name, bytes)
}

#[test]
fn native_vector_collection_input_retains_long_fields_through_post_eof_registration() {
    let (name, bytes) = fixture();
    let control = StorageReadControl::with_limit(2 << 20);
    let registry = MemoryCaptureRegistry::new();
    let mut decoder = input(&registry, &control, name.clone());
    let mut wire = Wire {
        bytes: std::io::Cursor::new(&bytes),
        end_reads: 0,
    };
    let mut post_eof = 0;
    let completed = loop {
        let before = wire.bytes.position();
        let result = decoder.advance(&mut wire, WINDOW).unwrap();
        assert!(wire.bytes.position() - before <= WINDOW.bytes as u64);
        if let Some(result) = result {
            break result;
        }
        post_eof += usize::from(wire.end_reads == 1);
    };
    assert!(post_eof >= 6, "field registration must retain its progress");
    assert_eq!(wire.end_reads, 1);
    let source = registry.cell(completed.finish().unwrap()).unwrap();
    assert_eq!(source.read().len(), 2);
    let capture_control = StorageReadControl::with_limit(2 << 20);
    let capture = registry.capture(&capture_control).unwrap();
    let selected = source.captured(&capture).unwrap();
    control.cancellation().cancel();
    source
        .write()
        .live_mut()
        .unwrap()
        .get_mut(name.as_ref())
        .unwrap()
        .add(19, vec![-1.0, 0.0])
        .unwrap();
    drop(source);
    let image = selected.image(&name, &capture).unwrap();
    drop((selected, capture, registry, decoder));
    let mut actual = Vec::new();
    image.write_image(&mut actual, &capture_control).unwrap();
    assert_eq!(actual, bytes);
    drop(image);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture_control.memory().used(), 0);
}

#[test]
fn native_vector_collection_input_retires_every_suspended_candidate() {
    let (name, bytes) = fixture();
    for boundary in 0..256 {
        let mut complete = false;
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(2 << 20);
            let registry = MemoryCaptureRegistry::new();
            let mut decoder = input(&registry, &control, name.clone());
            let mut wire = Wire {
                bytes: std::io::Cursor::new(&bytes),
                end_reads: 0,
            };
            for _ in 0..boundary {
                if let Some(result) = decoder.advance(&mut wire, WINDOW).unwrap() {
                    drop(result.finish().unwrap());
                    complete = true;
                    break;
                }
            }
            if !complete && cancel {
                control.cancellation().cancel();
                assert!(matches!(
                    decoder.advance(&mut wire, WINDOW),
                    Err(StorageBackendError::Cancelled(_))
                ));
                assert_eq!(control.memory().used(), 0);
                assert!(decoder.advance(&mut wire, WINDOW).is_err());
            }
            drop(decoder);
            assert_eq!(control.memory().used(), 0, "boundary {boundary}");
        }
        if complete {
            assert!(boundary > 30);
            return;
        }
    }
    panic!("native vector collection did not finish within the fixture bound");
}
