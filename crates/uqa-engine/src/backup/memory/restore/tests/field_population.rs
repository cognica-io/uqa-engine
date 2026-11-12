//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;
use uqa_core::image_io::ImageDecodeWindow;
use uqa_storage::backup::DecodedMemoryTableUnit;

struct OneEof<'a> {
    bytes: &'a [u8],
    eof: bool,
}
impl Read for OneEof<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "table preparation reread completed metadata");
        let count = self.bytes.read(output)?;
        self.eof = count == 0;
        Ok(count)
    }
}

fn before_preparation(
    units: &[Unit],
    destination: &input::TableDestination<'_>,
    control: &StorageReadControl,
) -> input::TableInput {
    let mut table =
        input::TableInput::start(&address(&units[0], control).unwrap(), control).unwrap();
    for (slot, unit) in units.iter().take(10).enumerate() {
        let mut reader = OneEof {
            bytes: &unit.bytes,
            eof: false,
        };
        loop {
            let (next, done) = table
                .read(
                    &DecodedMemoryTableUnit::Metadata(slot as u8),
                    &mut reader,
                    destination,
                    control,
                )
                .unwrap();
            table = next;
            if slot == 9 && reader.eof {
                assert!(!done);
                return table;
            }
            if done {
                break;
            }
        }
    }
    unreachable!("last metadata EOF retains table preparation")
}

#[test]
fn table_field_preparation_resumes_after_metadata_eof_and_retires_failed_candidates() {
    let source = source_with_historical_analyzers();
    let units = encoded_table(&source);
    let fields = source.durable.table_field_analyzers.read().clone();
    drop(source);
    let resources = uqa_analysis::AnalyzerResources::default();
    let temporary = DiskANNTemporaryBudget::new(8 << 20);
    for failure in [
        "none", "cancel", "memory", "window", "order", "finish", "drop",
    ] {
        let destination = Engine::new();
        let control = StorageReadControl::with_limit(32 << 20);
        let mut context = input::TableDestination {
            state: &destination.storage,
            fields: &fields,
            resources: &resources,
            maximum_value_bytes: 8 << 20,
            temporary: &temporary,
            window: ImageDecodeWindow { bytes: 1, steps: 1 },
        };
        let mut table = before_preparation(&units, &context, &control);
        let mut reader = OneEof {
            bytes: &[],
            eof: true,
        };
        let full = (failure == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        match failure {
            "drop" => drop(table),
            "finish" => {
                assert!(table.finish(&destination.storage, &control).is_err());
            }
            _ => {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                if failure == "window" {
                    context.window.steps = 0;
                }
                let slot = if failure == "order" { 8 } else { 9 };
                let mut calls = 0;
                loop {
                    calls += 1;
                    assert!(calls < 100);
                    if let Ok((next, done)) = table.read(
                        &DecodedMemoryTableUnit::Metadata(slot),
                        &mut reader,
                        &context,
                        &control,
                    ) {
                        table = next;
                        if done {
                            assert_eq!(failure, "none");
                            assert!(calls > 5);
                            drop(table);
                            break;
                        }
                    } else {
                        assert_ne!(failure, "none");
                        break;
                    }
                }
            }
        }
        assert!(destination.storage.tables.read().is_empty());
        drop((full, destination));
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}
