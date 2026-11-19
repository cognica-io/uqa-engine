//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::ImageDecodeWindow;

#[test]
fn table_metadata_input_retires_partial_units_before_table_publication() {
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text DEFAULT '한글'); INSERT INTO items VALUES(1,'original'); ANALYZE items", &[]).unwrap();
    let units = encoded_table(&source);
    drop(source);
    for stop in 1..=9 {
        for failure in ["cancel", "order", "drop"] {
            let control = StorageReadControl::with_limit(32 << 20);
            let properties = decoded_header(&units[0], &control).unwrap();
            let mut metadata = metadata::MetadataInput::new(properties);
            for slot in 1..stop {
                let mut reader = std::io::Cursor::new(&units[slot as usize].bytes);
                loop {
                    let (next, complete) = metadata
                        .read(
                            slot,
                            &mut reader,
                            ImageDecodeWindow {
                                bytes: 17,
                                steps: 3,
                            },
                            &control,
                        )
                        .unwrap();
                    metadata = next;
                    if complete {
                        break;
                    }
                }
            }
            let mut reader = std::io::Cursor::new(&units[stop as usize].bytes);
            let (metadata, complete) = metadata
                .read(
                    stop,
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: 64,
                    },
                    &control,
                )
                .unwrap();
            assert!(!complete);
            assert_eq!(reader.position(), 1);
            if failure == "drop" {
                drop(metadata);
            } else {
                let slot = if failure == "order" {
                    stop + 1
                } else {
                    control.cancellation().cancel();
                    stop
                };
                assert!(metadata
                    .read(
                        slot,
                        &mut reader,
                        ImageDecodeWindow { bytes: 1, steps: 1 },
                        &control
                    )
                    .is_err());
            }
            assert_eq!(control.memory().used(), 0, "{stop}: {failure}");
        }
    }
}
