//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::ColumnType,
    expr::composites::{CompositeAttribute, CompositeTypeDescriptor},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn retained_record() -> Value {
    let descriptor = CompositeTypeDescriptor {
        type_oid: 16_384,
        relation_oid: 16_385,
        dropped: vec![],
        attributes: (1..=3)
            .map(|number| CompositeAttribute {
                name: format!("f{number}"),
                number,
                ty: ColumnType::Integer,
                collation_oid: None,
            })
            .collect(),
    };
    let control = ProductionControl::uncontrolled();
    let value = crate::expr::composites::constructor::construct_current_with_control(
        &descriptor,
        None,
        &control,
        |index| Ok(control.copy_value(&Value::Int(index as i64 + 1))?),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    descriptor
        .expand_record_with_control(&value, &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap()
}

#[test]
fn expanded_field_selection_uses_its_original_layout_and_postgresql_type_error() {
    let value = retained_record();
    let mut field = CompositeFieldBinding {
        type_oid: 16_384,
        collation_oid: None,
        number: 1,
        result_type: ColumnType::BigInteger,
        dropped: false,
        changed_type: None,
    };
    let arguments = [value, Value::Str("renamed".into())];
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/retained_procedural_records.expected.json"
    )))
    .unwrap();
    let expected = &reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["sql"] == "SELECT retained_proc_widen_sql_fn() AS value")
        .unwrap()["error"];
    let error = select_with_control(&field, &arguments, None, &ProductionControl::uncontrolled())
        .unwrap_err();
    assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
    assert_eq!(error.to_string(), expected["message"].as_str().unwrap());
    assert_eq!(error.detail(), expected["detail"].as_str());
    field.number = 2;
    field.result_type = ColumnType::Integer;
    let selected =
        select_with_control(&field, &arguments, None, &ProductionControl::uncontrolled()).unwrap();
    let Value::Datum(selected) = &*selected else {
        panic!("deferred field");
    };
    assert_eq!(crate::expr::datums::read(selected).unwrap(), Value::Int(2));
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        select_with_control(&field, &arguments, None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert_eq!(
        select_with_control(&field, &arguments, None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}

#[test]
fn expanded_record_capture_admits_metadata_without_reading_invalid_addresses() {
    let value = retained_record();
    let Value::Datum(datum) = &value else {
        panic!("expanded record");
    };
    let ordinary = ProductionControl::uncontrolled();
    let descriptor = CompositeTypeDescriptor::expanded_with_control(datum, None, &ordinary)
        .unwrap()
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    let invalid = Value::Datum(datum.field(datum.type_oid(), u32::MAX));
    for limit in [0, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let result = descriptor.expand_record_with_control(&invalid, &control);
        if limit == 0 {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            let result = result.unwrap();
            let Value::Datum(expanded) = &*result else {
                panic!("expanded record");
            };
            assert_eq!(expanded.offset(), u32::MAX);
            assert_eq!(expanded.record_fields(), datum.record_fields());
            assert_eq!(result.reserved_bytes(), expanded.retained_bytes());
            drop(result);
        }
        assert_eq!(memory.used(), 0);
    }
}
