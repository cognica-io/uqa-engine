//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::window_tests::{assert_retirement, read_windowed, WINDOWS};
use crate::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::{
        image::{self, DefinitionImageReader},
        relation_attributes::DroppedAttribute,
        table_definition::TableDefinition,
        table_metadata::TableMetadata,
    },
    SQLError,
};
use uqa_core::{image_io::ImageDecoder, memory::MemoryBudget, CancellationToken};

fn bytes(mut reader: DefinitionImageReader<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 127];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

fn verify<T>(
    expected: &[u8],
    create: impl Fn(&MemoryBudget, &CancellationToken) -> Result<ImageDecoder<T, SQLError>, SQLError>,
    output: impl Fn(&T, &MemoryBudget, &CancellationToken) -> Vec<u8>,
) {
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(create(&memory, &token).unwrap(), expected, width, steps);
        token.cancel();
        assert_eq!(
            output(&restored, &memory, &CancellationToken::new()),
            expected
        );
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(expected, create);
}

#[test]
fn table_definition_input_resumes_original_columns_checks_and_hierarchy() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE saved(id integer DEFAULT 17, body text DEFAULT '한글', CHECK (id > 0)) INHERITS(parent)").unwrap().remove(0) else { panic!("table"); };
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let columns = bytes(image::columns_image_reader(&table.columns, &memory, &token).unwrap());
    let checks = bytes(image::table_checks_image_reader(&table.checks, &memory, &token).unwrap());
    let hierarchy =
        bytes(image::table_hierarchy_image_reader(&table.hierarchy, &memory, &token).unwrap());
    drop(table);
    verify(
        &columns,
        TableDefinition::<Vec<ColumnDef>>::image_input,
        |value, memory, token| bytes(image::columns_image_reader(value, memory, token).unwrap()),
    );
    verify(
        &checks,
        TableDefinition::<Vec<TableCheck>>::image_input,
        |value, memory, token| {
            bytes(image::table_checks_image_reader(value, memory, token).unwrap())
        },
    );
    verify(
        &hierarchy,
        TableDefinition::<TableHierarchy>::image_input,
        |value, memory, token| {
            bytes(image::table_hierarchy_image_reader(value, memory, token).unwrap())
        },
    );
}

#[test]
fn table_metadata_input_resumes_keys_foreign_keys_and_dropped_slots() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE saved(id integer PRIMARY KEY, parent integer, FOREIGN KEY(parent) REFERENCES original(id))").unwrap().remove(0) else { panic!("table"); };
    let dropped = vec![DroppedAttribute {
        number: 2,
        type_length: -1,
        type_modifier: -1,
        dimensions: 1,
        by_value: false,
        alignment: "i".into(),
        storage: "x".into(),
        collation: 100,
        identity: "d".into(),
        is_local: false,
        inheritance_count: 3,
    }];
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let keys =
        bytes(image::table_keys_image_reader(&table.key_constraints, &memory, &token).unwrap());
    let foreign =
        bytes(image::foreign_keys_image_reader(&table.foreign_keys, &memory, &token).unwrap());
    let dropped = bytes(image::dropped_attributes_image_reader(&dropped, &memory, &token).unwrap());
    drop(table);
    verify(
        &keys,
        TableMetadata::<Vec<TableKeyConstraint>>::image_input,
        |value, memory, token| bytes(image::table_keys_image_reader(value, memory, token).unwrap()),
    );
    verify(
        &foreign,
        TableMetadata::<Vec<ForeignKey>>::image_input,
        |value, memory, token| {
            bytes(image::foreign_keys_image_reader(value, memory, token).unwrap())
        },
    );
    verify(
        &dropped,
        TableMetadata::<Vec<DroppedAttribute>>::image_input,
        |value, memory, token| {
            bytes(image::dropped_attributes_image_reader(value, memory, token).unwrap())
        },
    );
}
