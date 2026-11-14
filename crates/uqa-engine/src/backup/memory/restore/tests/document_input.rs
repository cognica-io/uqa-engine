//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;
use uqa_storage::backup::DecodedMemoryTableUnit;

struct Reader<'a> {
    source: Cursor<&'a [u8]>,
    ended: bool,
}

impl Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            !self.ended,
            "document conversion reread its completed source"
        );
        let count = self.source.read(output)?;
        self.ended = count == 0;
        Ok(count)
    }
}

#[test]
fn table_restore_forwards_document_windows_through_native_conversion() {
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); INSERT INTO items VALUES(1, repeat('한글', 2048))", &[]).unwrap();
    let expected = source.sql("SELECT * FROM items", &[]).unwrap().rows;
    let units = encoded_table(&source);
    drop(source);
    let mut destination = Engine::new();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut table =
        input::TableInput::start(&address(&units[0], &control).unwrap(), &control).unwrap();
    let temporary = DiskANNTemporaryBudget::new(8 << 20);
    let resources = uqa_analysis::AnalyzerResources::default();
    {
        let fields = destination.durable.table_field_analyzers.read();
        let mut context = input::TableDestination {
            state: &destination.storage,
            fields: &fields,
            resources: &resources,
            maximum_value_bytes: 8 << 20,
            window: ImageDecodeWindow { bytes: 1, steps: 1 },
            temporary: &temporary,
        };
        let mut post_input = 0;
        for unit in &units {
            let DecodedMemoryImageAddress::Table {
                unit: component, ..
            } = address(unit, &control).unwrap()
            else {
                unreachable!()
            };
            let document = matches!(component, DecodedMemoryTableUnit::Document(_));
            let mut reader = Reader {
                source: Cursor::new(&unit.bytes),
                ended: false,
            };
            let mut calls = 0;
            loop {
                calls += 1;
                context.window.bytes = if calls % 2 == 0 { 1 } else { 17 };
                let before = reader.source.position();
                if document && reader.ended {
                    post_input += 1;
                }
                let (next, complete) = table
                    .read(&component, &mut reader, &context, &control)
                    .unwrap();
                table = next;
                if document {
                    assert!(reader.source.position() - before <= context.window.bytes as u64);
                }
                if complete {
                    break;
                }
            }
            assert_eq!(reader.source.position(), unit.bytes.len() as u64);
        }
        assert!(post_input > 10);
    }
    drop(units);
    let (name, table) = table.finish(&destination.storage, &control).unwrap();
    install(&mut destination, name, table, &control);
    control.cancellation().cancel();
    assert_eq!(
        destination.sql("SELECT * FROM items", &[]).unwrap().rows,
        expected
    );
    destination
        .sql(
            "INSERT INTO items VALUES(2, 'later'); UPDATE items SET body='changed' WHERE id=1",
            &[],
        )
        .unwrap();
    assert_eq!(
        destination
            .sql("SELECT id FROM items ORDER BY id", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    drop(destination);
    assert_eq!(control.memory().used(), 0);
}
