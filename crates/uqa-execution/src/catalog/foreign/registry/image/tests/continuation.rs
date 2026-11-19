//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::ImageDecodeWindow;

fn rich_image() -> Vec<u8> {
    let text = "한λ😀".repeat(1025);
    let uqa_sql::Statement::CreateTable(mut declaration) = uqa_sql::compile(&format!(
        "CREATE TABLE fields(payload text DEFAULT '{text}' CHECK (length(payload) > 0))"
    ))
    .unwrap()
    .remove(0) else {
        panic!("column declaration")
    };
    let mut definition = table();
    definition.columns[1].default = declaration.columns.remove(0).default;
    definition.options.insert("large".into(), text);
    definition.option_order.push("large".into());
    let mut source = ForeignCatalog::default();
    source.insert(key(), definition, authority()).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let selected = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drop(source);
    collect(&selected, 19)
}

#[test]
fn native_foreign_input_resumes_nested_fields_with_paired_original_owners() {
    let expected = rich_image();
    for width in [1, 17, 4096] {
        let input = MemoryBudget::new(8 << 20);
        let mutations = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let mut decoder = ForeignCatalog::image_input(&input, &mutations, &token).unwrap();
        let mut reader = Cursor::new(expected.as_slice());
        let mut restored = loop {
            let before = reader.position();
            let result = decoder
                .advance(
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: width,
                        steps: 3,
                    },
                )
                .unwrap();
            assert!(reader.position() - before <= width as u64);
            if let Some(value) = result {
                break value;
            }
        };
        assert_eq!(reader.position(), expected.len() as u64);
        let selected = restored.clone();
        let occupied = input.reserve(input.available()).unwrap();
        token.cancel();
        restored.replace_table(key(), table()).unwrap();
        restored
            .replace_security(&key(), BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP))
            .unwrap();
        let output = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        assert_eq!(
            collect(
                &selected
                    .capture(&ProductionControl::new(&output, &token, &token))
                    .unwrap(),
                13
            ),
            expected
        );
        assert!(restored.remove(&key()).unwrap());
        assert!(restored.tables().is_empty() && restored.security().is_empty());
        drop(restored);
        assert_eq!(mutations.used(), 0);
        assert!(input.used() > occupied.bytes());
        drop(selected);
        assert_eq!(input.used(), occupied.bytes());
        drop(occupied);
        assert_eq!(input.used(), 0);
    }
}

#[test]
fn native_foreign_input_retires_partial_children_on_failure_cancellation_and_drop() {
    let bytes = rich_image();
    for offset in [1, 128, bytes.len() / 2, bytes.len() - 1] {
        for mode in [0, 1, 2] {
            let input = MemoryBudget::new(8 << 20);
            let mutations = MemoryBudget::new(8 << 20);
            let token = CancellationToken::new();
            let mut decoder = ForeignCatalog::image_input(&input, &mutations, &token).unwrap();
            let mut reader = Cursor::new(bytes.as_slice());
            while reader.position() < offset as u64 {
                assert!(decoder
                    .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 3 })
                    .unwrap()
                    .is_none());
            }
            match mode {
                0 => {
                    token.cancel();
                    assert!(decoder
                        .advance(
                            &mut reader,
                            ImageDecodeWindow {
                                bytes: 17,
                                steps: 3
                            }
                        )
                        .is_err());
                }
                1 => loop {
                    match decoder.advance(
                        &mut &[][..],
                        ImageDecodeWindow {
                            bytes: 17,
                            steps: 3,
                        },
                    ) {
                        Ok(None) => (),
                        Err(_) => break,
                        Ok(Some(_)) => panic!("incomplete foreign image accepted"),
                    }
                },
                _ => (),
            }
            drop(decoder);
            assert_eq!(input.used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
}

#[test]
fn native_foreign_input_admits_continuations_and_payload_before_allocation() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    let mutations = MemoryBudget::new(8 << 20);
    let decoder = ForeignCatalog::image_input(&memory, &mutations, &token).unwrap();
    let header = memory.used();
    drop(decoder);
    let denied = MemoryBudget::new(header - 1);
    assert!(ForeignCatalog::image_input(&denied, &mutations, &token).is_err());
    assert_eq!(denied.used(), 0);
    let decoder = ForeignCatalog::image_input(&memory, &mutations, &token).unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    let Err(error) = decoder.read(&mut rich_image().as_slice()) else {
        panic!("unadmitted payload accepted")
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
}
