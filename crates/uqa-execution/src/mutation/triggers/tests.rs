//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn validated_trigger_fields_preserve_every_admitted_byte() {
    let columns = [("c".into(), uqa_sql::ColumnType::InternalChar)];
    for byte in 0..=u8::MAX {
        let signed = i64::from(i8::from_ne_bytes([byte]));
        let value =
            uqa_sql::expr::cast_value_from(&Value::Int(signed), "\"char\"", Some("integer"))
                .unwrap();
        let record = Value::Record(vec![("c".into(), value.clone())].into());
        let actual = normalize_instead_of_trigger_record(&columns, record)
            .unwrap()
            .unwrap();
        assert_eq!(actual, [value]);
    }
    assert!(normalize_instead_of_trigger_record(&columns, Value::Null)
        .unwrap()
        .is_none());
}
