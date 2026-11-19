//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Reader {
    bytes: Cursor<Vec<u8>>,
    eof: bool,
}

impl Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "catalog replayed a completed model");
        let count = self.bytes.read(output)?;
        self.eof = count == 0;
        Ok(count)
    }
}

fn encoded() -> Vec<u8> {
    let mut source = (*model(1)).clone();
    source
        .layers
        .extend(std::iter::repeat_n(DeepLayerSpec::Flatten, 128));
    let mut bytes = Vec::new();
    crate::model_image::write_model_image(&source, &mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn key(control: &StorageReadControl) -> Budgeted<String> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text("saved\0한글")
    .unwrap()
    .into_budgeted()
    .unwrap()
}

fn input(
    control: &StorageReadControl,
    key: Budgeted<String>,
) -> ImageDecoder<ModelCatalog, StorageBackendError> {
    let mut catalog = ModelCatalog::new(control.memory());
    catalog.insert("prior".into(), model(1)).unwrap();
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        move |mut channel| async move { catalog.read_image_from(key, &mut channel.input()).await },
    )
    .unwrap()
}

#[test]
fn native_model_input_resumes_footprint_and_publication_with_original_name_admission() {
    let bytes = encoded();
    let control = StorageReadControl::with_limit(1 << 20);
    let name = key(&control);
    let original_name = name.as_ptr();
    let mut input = input(&control, name);
    let mut reader = Reader {
        bytes: Cursor::new(bytes.clone()),
        eof: false,
    };
    let mut after_eof = 0;
    let catalog = loop {
        after_eof += usize::from(reader.eof);
        if let Some(catalog) = input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 17,
                    steps: 1,
                },
            )
            .unwrap()
        {
            break catalog;
        }
        assert!(after_eof < 1024);
    };
    assert!(
        after_eof > 260,
        "validation and footprint must both retain per-layer progress"
    );
    assert_eq!(
        catalog
            .values
            .iter()
            .find(|(name, _)| name.as_str() == "saved\0한글")
            .unwrap()
            .0
            .as_ptr(),
        original_name
    );
    let ordinary = StorageReadControl::with_limit(1 << 20);
    let mut synchronous = ModelCatalog::new(ordinary.memory());
    synchronous.insert("prior".into(), model(1)).unwrap();
    let synchronous = synchronous
        .read_image(key(&ordinary), &mut bytes.as_slice(), &ordinary)
        .unwrap();
    assert_eq!(control.memory().used(), ordinary.memory().used());
    drop(synchronous);
    assert_eq!(ordinary.memory().used(), 0);
    let runtime = MemoryBudget::new(65536);
    let mut catalog = catalog.into_mutation_budget(&runtime);
    let retained = catalog.get("saved\0한글").unwrap().clone();
    control.cancellation().cancel();
    catalog.insert("later".into(), model(1)).unwrap();
    let captured_control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&captured_control).unwrap();
    let image = captured.image("saved\0한글").unwrap().unwrap();
    assert_eq!(super::image(&image), bytes);
    catalog.remove("saved\0한글").unwrap();
    drop((catalog, captured, input));
    assert_eq!(runtime.used(), 0);
    assert!(control.memory().used() > 0);
    drop((retained, image));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(captured_control.memory().used(), 0);
}

#[test]
fn native_model_input_retires_the_entire_candidate_after_eof_interruption() {
    let bytes = encoded();
    for mode in 0..4 {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = input(&control, key(&control));
        let mut reader = Reader {
            bytes: Cursor::new(bytes.clone()),
            eof: false,
        };
        while !reader.eof {
            assert!(input
                .advance(
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: 4096,
                        steps: 1
                    }
                )
                .unwrap()
                .is_none());
        }
        let held = if mode == 2 {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        if mode == 0 {
            control.cancellation().cancel();
        }
        if mode != 1 {
            loop {
                match input.advance(
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: usize::from(mode != 3),
                        steps: 1,
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("interrupted candidate was published"),
                    Err(_) => break,
                }
            }
            assert!(input
                .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        drop((input, held));
        assert_eq!(control.memory().used(), 0, "interruption {mode}");
    }
}
