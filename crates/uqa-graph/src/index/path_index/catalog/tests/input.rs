//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_work::tests::Reader;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

#[test]
fn path_catalog_input_keeps_original_name_and_retires_post_eof_cancellation() {
    let source = materialized("edge");
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    drop(source);
    let key = "catalog\0한글".repeat(1025);
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(4 << 20);
        let name = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        )
        .copy_text(&key)
        .unwrap()
        .into_budgeted()
        .unwrap();
        let pointer = name.as_ptr();
        let catalog = PathIndexCatalog::new(control.memory());
        let mut decoder =
            ImageDecoder::<_, StorageBackendError>::new(
                control.memory(),
                control.cancellation(),
                move |mut channel| async move {
                    catalog.read_image_from(name, &mut channel.input()).await
                },
            )
            .unwrap();
        let mut reader = Reader::new(&bytes);
        let mut after_eof = 0;
        loop {
            if reader.eof {
                after_eof += 1;
                if cancel {
                    control.cancellation().cancel();
                }
            }
            match decoder.advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 17,
                    steps: 1,
                },
            ) {
                Ok(None) => (),
                Ok(Some(catalog)) => {
                    assert!(!cancel && after_eof > 1);
                    assert_eq!(catalog.keys().next().unwrap().as_ptr(), pointer);
                    control.cancellation().cancel();
                    let mut restored = Vec::new();
                    catalog
                        .get(&key)
                        .unwrap()
                        .write_image(&mut restored, &StorageReadControl::with_limit(0))
                        .unwrap();
                    assert_eq!(restored, bytes);
                    drop(catalog);
                    break;
                }
                Err(error) => {
                    assert!(cancel && reader.eof);
                    assert!(matches!(error, StorageBackendError::Cancelled(_)));
                    break;
                }
            }
        }
        assert_eq!(control.memory().used(), 0);
    }
}
