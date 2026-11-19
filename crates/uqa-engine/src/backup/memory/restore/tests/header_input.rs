//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

struct Reader<'a> {
    bytes: Cursor<&'a [u8]>,
    ended: bool,
}

impl Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.ended, "header conversion reread the completed source");
        let count = self.bytes.read(output)?;
        self.ended = count == 0;
        Ok(count)
    }
}

fn wide_table() -> Vec<Unit> {
    let source = Engine::new();
    source
        .create_table(
            "items",
            uqa_analysis::Analyzer {
                tokenizer: uqa_analysis::Tokenizer::Keyword,
                token_filters: vec![uqa_analysis::TokenFilter::Stop {
                    language: "english".into(),
                    custom_words: (0..128)
                        .map(|i| format!("{i:04}{}", "한글".repeat(128)))
                        .collect(),
                }],
                ..uqa_analysis::Analyzer::default()
            },
            (0..128).map(|i| format!("field_{i:04}")).collect(),
        )
        .unwrap();
    encoded_table(&source)
}

#[test]
fn table_header_conversion_yields_after_eof_before_admitting_the_next_metadata() {
    let units = wide_table();
    let control = StorageReadControl::with_limit(32 << 20);
    let DecodedMemoryImageAddress::Table {
        object, generation, ..
    } = address(&units[0], &control).unwrap()
    else {
        unreachable!()
    };
    let mut input = metadata::header::input(object, generation, &control).unwrap();
    let mut reader = Reader {
        bytes: Cursor::new(&units[0].bytes),
        ended: false,
    };
    let mut post_input = 0;
    let header = loop {
        if reader.ended {
            post_input += 1;
        }
        if let Some(value) = input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 8192,
                    steps: 1,
                },
            )
            .unwrap()
        {
            break value;
        }
    };
    assert!(post_input > 128);
    assert_eq!(header.fts_fields.len(), 128);
    assert_eq!(header.fts_fields[127], "field_0127");
    assert_eq!(header.array_name.as_deref(), Some("_items"));
    assert!(matches!(
        header.analyzer.configuration().token_filters[0],
        uqa_analysis::TokenFilter::Stop { .. }
    ));
    drop((header, input));
    assert_eq!(control.memory().used(), 0);
    let mut restored = Engine::new();
    let (name, table) = reconstruct(&restored, &units, &control).unwrap();
    install(&mut restored, name, table, &control);
    let reconstructed = encoded_table(&restored);
    assert_eq!(reconstructed.len(), units.len());
    assert!(reconstructed
        .iter()
        .zip(&units)
        .all(|(a, b)| a.address == b.address && a.bytes == b.bytes));
    restored
        .add_document(
            "items",
            1,
            [("field_0000".into(), uqa_core::Value::Str("retained".into()))].into(),
        )
        .unwrap();
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn table_header_conversion_retires_private_state_on_cancel_denial_and_wrong_order() {
    let units = wide_table();
    for failure in ["cancel", "memory", "window", "order", "drop"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let destination = Engine::new();
        let resources = uqa_analysis::AnalyzerResources::default();
        let fields = destination.durable.table_field_analyzers.read();
        let temporary = DiskANNTemporaryBudget::new(8 << 20);
        let mut context = input::TableDestination {
            state: &destination.storage,
            fields: &fields,
            resources: &resources,
            maximum_value_bytes: 8 << 20,
            window: ImageDecodeWindow {
                bytes: 8192,
                steps: 1,
            },
            temporary: &temporary,
        };
        let mut table =
            input::TableInput::start(&address(&units[0], &control).unwrap(), &control).unwrap();
        let mut reader = Reader {
            bytes: Cursor::new(&units[0].bytes),
            ended: false,
        };
        while !reader.ended {
            let (next, complete) = table
                .read(
                    &uqa_storage::backup::DecodedMemoryTableUnit::Metadata(0),
                    &mut reader,
                    &context,
                    &control,
                )
                .unwrap();
            assert!(!complete);
            table = next;
        }
        assert!(table.reading_header());
        if failure == "drop" {
            drop(table);
        } else {
            let full = if failure == "memory" {
                Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                )
            } else {
                None
            };
            if failure == "cancel" {
                control.cancellation().cancel();
            }
            if failure == "window" {
                context.window.steps = 0;
            }
            let slot =
                uqa_storage::backup::DecodedMemoryTableUnit::Metadata(u8::from(failure == "order"));
            loop {
                match table.read(&slot, &mut reader, &context, &control) {
                    Ok((next, false)) => table = next,
                    Ok((_, true)) => panic!("incomplete header published: {failure}"),
                    Err(_) => break,
                }
            }
            drop(full);
        }
        assert_eq!(control.memory().used(), 0, "{failure}");
        assert!(destination.storage.tables.read().is_empty());
    }
}
