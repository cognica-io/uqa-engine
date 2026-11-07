//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    AutoIncrementKind, AutoIncrementOwner, ColumnType, ConstraintCatalogIdentity, Expr,
    IdentitySequenceName, RelationPersistence, SequenceOptionValue,
};
use crate::catalog::image::read_columns_image;
use uqa_core::Value;

mod ownership;

fn collect(mut reader: DefinitionImageReader<'_>, size: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0; size];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    bytes
}

fn encode(columns: &[ColumnDef], size: usize) -> Vec<u8> {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let bytes = collect(
        columns_image_reader(columns, &memory, &token).unwrap(),
        size,
    );
    assert_eq!(memory.used(), 0);
    bytes
}

fn literal() -> (ColumnDef, Vec<u8>) {
    let column = ColumnDef {
        collation_oid: Some(950),
        object_id: Some([11; 16]),
        attribute_number: Some(7),
        primary_key: true,
        not_null: true,
        not_null_explicit: true,
        not_null_validated: false,
        not_null_no_inherit: true,
        not_null_is_local: false,
        unique: true,
        check_enforced: false,
        check_validated: true,
        check_no_inherit: true,
        check_is_local: false,
        not_null_name: Some("nn".into()),
        not_null_identity: Some(ConstraintCatalogIdentity {
            object_id: [12; 16],
            oid: 65536,
        }),
        check_name: Some("ck".into()),
        check_object_id: Some([13; 16]),
        check_catalog_oid: Some(65537),
        default_catalog_oid: Some(65538),
        default: Some(Expr::Star),
        generated: Some(GeneratedColumn {
            kind: GeneratedColumnKind::Stored,
            expression: Box::new(Expr::Not(Box::new(Expr::Star))),
            function_dependencies: vec![],
        }),
        check: Some(Expr::Default),
        ..ColumnDef::nullable("v", ColumnType::Integer)
    };
    let mut bytes = b"UQACOL01\x01\0\0\0\0\0\0\0".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(b'v');
    bytes.push(1);
    bytes.extend(950u32.to_le_bytes());
    bytes.push(1);
    bytes.extend([11; 16]);
    bytes.push(1);
    bytes.extend(7i16.to_le_bytes());
    bytes.extend([1, 1, 1, 0, 1, 0, 1, 0, 1, 1, 0, 0, 0, 1, 1, 1, 0]);
    bytes.push(1); // INTEGER.
    bytes.push(1);
    bytes.extend(2u64.to_le_bytes());
    bytes.extend(b"nn");
    bytes.push(1);
    bytes.extend([12; 16]);
    bytes.extend(65536i64.to_le_bytes());
    bytes.push(1);
    bytes.extend(2u64.to_le_bytes());
    bytes.extend(b"ck");
    bytes.push(1);
    bytes.extend([13; 16]);
    bytes.push(1);
    bytes.extend(65537i64.to_le_bytes());
    bytes.push(1);
    bytes.extend(65538i64.to_le_bytes());
    bytes.push(0); // STAR default.
    bytes.push(1); // STORED generated expression; no function dependencies.
    bytes.extend(0u64.to_le_bytes());
    bytes.extend([15, 0, 2]); // NOT STAR, then DEFAULT CHECK.
    (column, bytes)
}

#[test]
fn native_column_images_match_independent_physical_attribute_bytes() {
    let (column, bytes) = literal();
    for size in [1, 13, 128, 20000] {
        assert_eq!(encode(std::slice::from_ref(&column), size), bytes);
    }
    let memory = MemoryBudget::new(1 << 20);
    let decoded =
        read_columns_image(&mut bytes.as_slice(), &memory, &CancellationToken::new()).unwrap();
    assert_eq!(
        serde_json::to_value(&*decoded).unwrap(),
        serde_json::to_value([column]).unwrap()
    );
    assert_eq!(encode(&decoded, 37), bytes);
    drop(decoded);
    assert_eq!(memory.used(), 0);
}

fn complete() -> ColumnDef {
    let mut column = literal().0;
    column.missing_value = Some(Value::Bytes(vec![0, 255, 17]));
    column.auto_increment = Some(AutoIncrement {
        kind: AutoIncrementKind::IdentityByDefault,
        sequence: Some("old.sequence".into()),
        owner: Some(AutoIncrementOwner {
            table: "old.table".into(),
            column: "old.column".into(),
        }),
        declaration: Some(Box::new(IdentitySequenceDeclaration {
            name: Some(IdentitySequenceName {
                schema: Some("s".into()),
                name: "seq".into(),
            }),
            persistence: Some(RelationPersistence::Unlogged),
            sequence: SequenceDeclaration {
                data_type: Some(ColumnType::Array(Box::new(ColumnType::BigInteger))),
                increment: Some(SequenceOptionValue::Text("+3.0".into())),
                max_value: Some(SequenceOptionValue::Absent),
                min_value: Some(SequenceOptionValue::Integer(-17)),
                start: None,
                restart: Some(SequenceOptionValue::Integer(19)),
                cache: Some(SequenceOptionValue::Text("1e2".into())),
                cycle: Some(false),
                owned_by: Some(vec!["old.schema".into(), "old.table".into(), "v".into()]),
            },
            error: Some(DeferredSQLError {
                sqlstate: "22003".into(),
                message: "original failure".into(),
                detail: Some(String::new()),
                hint: Some("original hint".into()),
            }),
        })),
    });
    column.generated.as_mut().unwrap().function_dependencies = vec![FunctionBinding {
        object_id: Some([21; 16]),
        name: "old.fn".into(),
        argument_types: vec!["old.type".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }];
    column.references = Some(ForeignKeyRef {
        referenced_index: Some([22; 16]),
        referenced_key: Some("old.key".into()),
        name: Some("fk".into()),
        object_id: Some([23; 16]),
        catalog_identity: Some(ConstraintCatalogIdentity {
            object_id: [24; 16],
            oid: 65540,
        }),
        table: "old.parent".into(),
        column: Some("pk".into()),
        on_update: ForeignKeyAction::Cascade,
        on_delete: ForeignKeyAction::SetDefault,
        match_type: ForeignKeyMatch::Full,
        enforced: false,
        validated: true,
        deferrable: true,
        initially_deferred: false,
        period: true,
        referenced_partitions: vec![ReferencedPartitionConstraint {
            partition: [25; 16],
            parent: Some([26; 16]),
            name: "original_partition_fk".into(),
            catalog_identity: ConstraintCatalogIdentity {
                object_id: [27; 16],
                oid: 65541,
            },
            validated: false,
        }],
    });
    column
}

#[test]
fn native_column_images_preserve_declarations_and_optional_metadata() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for mask in 0..128 {
        let mut column = complete();
        if mask & 1 == 0 {
            column.missing_value = None;
            column.object_id = None;
            column.attribute_number = None;
            column.not_null_identity = None;
            column.check_object_id = None;
        }
        if mask & 2 == 0 {
            column.default = None;
            column.check = None;
            column.check_catalog_oid = None;
            column.default_catalog_oid = None;
        }
        if mask & 4 == 0 {
            column.generated = None;
            column.collation_oid = None;
            column.not_null_name = None;
            column.check_name = None;
        }
        if mask & 8 == 0 {
            column.references = None;
        }
        if mask & 16 == 0 {
            column.auto_increment = None;
        }
        if let Some(auto) = &mut column.auto_increment {
            auto.kind = [
                AutoIncrementKind::Serial,
                AutoIncrementKind::IdentityAlways,
                AutoIncrementKind::IdentityByDefault,
                AutoIncrementKind::Legacy,
            ][mask % 4];
            if mask & 32 == 0 {
                auto.declaration = None;
            }
            if mask & 64 == 0 {
                auto.owner = None;
                auto.sequence = None;
            }
        }
        let columns = vec![column, ColumnDef::nullable("", ColumnType::Text)];
        let bytes = encode(&columns, 127);
        let restored = read_columns_image(&mut bytes.as_slice(), &memory, &token).unwrap();
        assert_eq!(
            serde_json::to_value(&*restored).unwrap(),
            serde_json::to_value(&columns).unwrap(),
            "mask {mask}"
        );
        assert_eq!(encode(&restored, 31), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    let zero = MemoryBudget::new(0);
    let bytes = b"UQACOL01\0\0\0\0\0\0\0\0";
    assert!(read_columns_image(&mut bytes.as_slice(), &zero, &token)
        .unwrap()
        .is_empty());
}
