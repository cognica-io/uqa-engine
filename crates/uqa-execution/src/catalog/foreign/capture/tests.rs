//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn definition() -> StoredForeignTable {
    let uqa_sql::Statement::CreateTable(mut declaration) = uqa_sql::compile(
        "CREATE TABLE fields(id integer, payload text DEFAULT 'original', CONSTRAINT valid CHECK(length(payload)>0))",
    ).unwrap().remove(0) else { panic!("table") };
    for (index, column) in declaration.columns.iter_mut().enumerate() {
        column.object_id = Some([u8::try_from(index + 4).unwrap(); 16]);
        column.attribute_number = Some(i16::try_from(index + 2).unwrap());
    }
    declaration.columns[1].collation_oid = Some(100);
    declaration.columns[1].missing_value =
        Some(Value::Float(f64::from_bits(0xfff8_0000_0000_0007)));
    StoredForeignTable {
        name: "\"Quoted.Schema\".\"Foreign.Name\"".into(),
        persistence: RelationPersistence::Permanent,
        object_id: [1; 16],
        catalog_oids: Some(RelationCatalogOids {
            relation: 24001,
            row_type: Some(24002),
            array_type: Some(24003),
            rule: None,
        }),
        row_type_array_name: Some("_Foreign.Name".into()),
        server_name: "Original.Server".into(),
        server_reference: Some(ForeignServerReference {
            oid: 24004,
            object_id: [2; 16],
        }),
        dropped_attributes: vec![DroppedAttribute::from_column(
            &ColumnDef::nullable("gone", uqa_sql::ColumnType::Text),
            0,
            true,
            0,
        )
        .unwrap()],
        columns: declaration.columns,
        checks: declaration.checks,
        options: BTreeMap::from([
            ("z".into(), "first\nvalue".into()),
            ("a".into(), "second 한글".into()),
        ]),
        option_order: vec!["z".into(), "a".into()],
    }
}

#[test]
fn foreign_table_capture_keeps_complete_declarations_slots_references_and_option_order() {
    let value = definition();
    let source = BTreeMap::from([(
        RelationIdentity::new("Quoted.Schema", "Foreign.Name"),
        value,
    )]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let actual = allocation_counter::measure(|| saved = Some(tables(&source, &control).unwrap()));
    let saved = saved.unwrap();
    let (key, value) = &saved[0];
    let (original_key, original) = source.first_key_value().unwrap();
    assert_eq!(key, original_key);
    assert_eq!(value.name, original.name);
    assert_eq!(value.persistence, original.persistence);
    assert_eq!(value.object_id, original.object_id);
    assert_eq!(value.catalog_oids, original.catalog_oids);
    assert_eq!(value.row_type_array_name, original.row_type_array_name);
    assert_eq!(value.server_name, original.server_name);
    assert_eq!(value.server_reference, original.server_reference);
    assert_eq!(value.dropped_attributes, original.dropped_attributes);
    assert_eq!(value.option_order, original.option_order);
    assert!(value
        .options
        .iter()
        .map(|(k, v)| (k, v))
        .eq(original.options.iter()));
    assert_eq!(
        serde_json::to_value(&value.columns).unwrap(),
        serde_json::to_value(&original.columns).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&value.checks).unwrap(),
        serde_json::to_value(&original.checks).unwrap()
    );
    assert!(value.columns[1]
        .missing_value
        .as_ref()
        .unwrap()
        .has_same_representation(original.columns[1].missing_value.as_ref().unwrap()));
    assert_eq!(actual.bytes_current, i64::try_from(memory.used()).unwrap());
    drop(source);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn foreign_table_capture_detaches_retained_defaults_from_the_source_allowance() {
    let original = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let input = ProductionControl::new(&original, &source_token, &source_token);
    let value = input.copy_value(&Value::Str("x".repeat(32768))).unwrap();
    let value = uqa_sql::ir::RetainedConstantValue::from_produced(value, &input).unwrap();
    let literal =
        uqa_sql::ir::RetainedScalarLiteral::with_control(value, None, None, None, &input).unwrap();
    let mut table = definition();
    table.columns[1].default = Some(uqa_sql::ast::Expr::RetainedLiteral(Box::new(
        uqa_sql::ast::RetainedInputLiteral(literal),
    )));
    let source = BTreeMap::from([(RelationIdentity::new("s", "t"), table)]);
    source_token.cancel();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let saved = tables(&source, &control).unwrap();
    drop(source);
    assert_eq!(original.used(), 0);
    let Some(uqa_sql::ast::Expr::RetainedLiteral(literal)) = &saved[0].1.columns[1].default else {
        panic!("retained default")
    };
    assert!(
        matches!(literal.0.value.resident_value(), Some(Value::Str(value)) if value.len()==32768)
    );
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn foreign_table_capture_rejects_late_options_and_cancellation_without_partial_retention() {
    let mut value = definition();
    value.options.insert("large".into(), "x".repeat(32768));
    let source = BTreeMap::from([(RelationIdentity::new("s", "t"), value)]);
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        tables(&source, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        tables(&source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
