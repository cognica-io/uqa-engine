//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{MemoryBudget, MemoryError};

fn tagged(
    tag: &str,
    fields: impl IntoIterator<Item = (&'static str, Value)>,
) -> BTreeMap<String, Value> {
    std::iter::once(("$uqa_type".to_owned(), Value::Str(tag.to_owned())))
        .chain(fields.into_iter().map(|(name, value)| (name.into(), value)))
        .collect()
}

fn payload(value: &Value) -> usize {
    value
        .retained_payload_bytes(&MemoryBudget::new(1 << 20), &CancellationToken::new())
        .unwrap()
}

fn retained_map(
    map: BTreeMap<String, Value>,
    memory: &MemoryBudget,
) -> Budgeted<BTreeMap<String, Value>> {
    let value = Value::Map(map);
    let reservation = memory.reserve(payload(&value)).unwrap();
    let Value::Map(map) = value else {
        unreachable!()
    };
    Budgeted::new(map, reservation)
}

fn spare_text(text: &str) -> String {
    let mut value = String::with_capacity(text.len() + 128);
    value.push_str(text);
    value
}

#[test]
fn controlled_tags_preserve_ordinary_and_serde_semantics_with_exact_final_leases() {
    let cases = [
        r#"{"$uqa_type":"void"}"#,
        r#"{"$uqa_type":"date","days":-12}"#,
        r#"{"$uqa_type":"time","micros":123}"#,
        r#"{"$uqa_type":"time_tz","micros":123,"offset_minutes":-90}"#,
        r#"{"$uqa_type":"timestamp","micros":-123}"#,
        r#"{"$uqa_type":"timestamp_tz","micros":123}"#,
        r#"{"$uqa_type":"interval","months":2,"days":-4,"micros":9}"#,
        r#"{"$uqa_type":"decimal","value":"1e4096","ignored":"extra"}"#,
        r#"{"$uqa_type":"decimal","value":"NaN"}"#,
        r#"{"$uqa_type":"decimal","value":"invalid"}"#,
        r#"{"$uqa_type":"fixed_char","value":"a  "}"#,
        r#"{"$uqa_type":"json","value":" {\"a\": 1} "}"#,
        r#"{"$uqa_type":"jsonb","value":"{\"a\":1}"}"#,
        r#"{"$uqa_type":"bytes","hex":"00FfaB"}"#,
        r#"{"$uqa_type":"bytes","hex":"0z"}"#,
        r#"{"$uqa_type":"bytes","hex":"0"}"#,
        r#"{"$uqa_type":"row","values":[1,"text",null]}"#,
        r#"{"$uqa_type":"record","fields":[["same",1],["same",[2]]]}"#,
        r#"{"$uqa_type":"array","values":[],"lower_bounds":[]}"#,
        r#"{"$uqa_type":"array","values":[[],[]],"lower_bounds":[-2,4]}"#,
        r#"{"$uqa_type":"array","values":[[1,2],[3,4]],"lower_bounds":[-2,4]}"#,
        r#"{"$uqa_type":"array","values":[[1],2],"lower_bounds":[1,1]}"#,
        r#"{"$uqa_type":"array","values":[1],"lower_bounds":[]}"#,
        r#"{"$uqa_type":"array","values":[1],"lower_bounds":[2147483648]}"#,
        r#"{"$uqa_type":"record","fields":[["valid",1],[2,3]]}"#,
        r#"{"$uqa_type":"void","extra":true}"#,
        r#"{"$uqa_type":"date","days":1,"extra":true}"#,
        r#"{"$uqa_type":"unknown","value":[1,2]}"#,
        r#"{"$uqa_type":1,"value":"untagged"}"#,
    ];
    for encoded in cases {
        let map: BTreeMap<String, Value> = serde_json::from_str(encoded).unwrap();
        let expected: Value = serde_json::from_str(encoded).unwrap();
        assert_eq!(value_from_tagged_map(map.clone()).unwrap(), expected);
        let memory = MemoryBudget::new(1 << 20);
        let decoded =
            value_from_tagged_map_budgeted(retained_map(map, &memory), &CancellationToken::new())
                .unwrap();
        assert_eq!(*decoded, expected, "{encoded}");
        assert_eq!(decoded.reserved_bytes(), payload(&decoded), "{encoded}");
        assert_eq!(memory.used(), decoded.reserved_bytes());
        assert_eq!(
            serde_json::to_vec(&*decoded).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn controlled_text_and_row_tags_move_existing_buffers_without_payload_reallocation() {
    for tag in ["fixed_char", "json", "jsonb"] {
        let text = spare_text("payload");
        let pointer = text.as_ptr();
        let capacity = text.capacity();
        let value = Value::Map(tagged(tag, [("value", Value::Str(text))]));
        let input_bytes = payload(&value);
        let Value::Map(map) = value else {
            unreachable!()
        };
        let memory = MemoryBudget::new(input_bytes);
        let decoded =
            value_from_tagged_map_budgeted(retained_map(map, &memory), &CancellationToken::new())
                .unwrap();
        let (Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text)) = &*decoded else {
            panic!("expected a text tag")
        };
        assert_eq!(text.as_ptr(), pointer);
        assert_eq!(text.capacity(), capacity);
        assert_eq!(decoded.reserved_bytes(), capacity);
        assert_eq!(memory.peak(), input_bytes);
    }

    let text = spare_text("row payload");
    let text_pointer = text.as_ptr();
    let mut row = Vec::with_capacity(12);
    row.push(Value::Str(text));
    let pointer = row.as_ptr();
    let capacity = row.capacity();
    let memory = MemoryBudget::new(1 << 20);
    let decoded = value_from_tagged_map_budgeted(
        retained_map(tagged("row", [("values", Value::List(row))]), &memory),
        &CancellationToken::new(),
    )
    .unwrap();
    let Value::Row(row) = &*decoded else {
        panic!("expected a row")
    };
    assert_eq!(row.as_ptr(), pointer);
    assert_eq!(row.capacity(), capacity);
    let Value::Str(text) = &row[0] else {
        panic!("expected row text")
    };
    assert_eq!(text.as_ptr(), text_pointer);
    assert_eq!(decoded.reserved_bytes(), payload(&decoded));
}

#[test]
fn controlled_records_and_arrays_move_names_values_and_normalized_element_buffers() {
    let name = spare_text("same");
    let name_pointer = name.as_ptr();
    let text = spare_text("nested");
    let text_pointer = text.as_ptr();
    let fields = vec![
        Value::List(vec![Value::Str(name), Value::Str(text)]),
        Value::List(vec![Value::Str("same".into()), Value::Int(2)]),
    ];
    let memory = MemoryBudget::new(1 << 20);
    let decoded = value_from_tagged_map_budgeted(
        retained_map(tagged("record", [("fields", Value::List(fields))]), &memory),
        &CancellationToken::new(),
    )
    .unwrap();
    let Value::Record(fields) = &*decoded else {
        panic!("expected a record")
    };
    assert_eq!(fields[0].0.as_ptr(), name_pointer);
    assert_eq!(fields[0].0, fields[1].0);
    let Value::Str(text) = &fields[0].1 else {
        panic!("expected record text")
    };
    assert_eq!(text.as_ptr(), text_pointer);
    assert_eq!(decoded.reserved_bytes(), payload(&decoded));
    drop(decoded);
    assert_eq!(memory.used(), 0);

    let mut inner = Vec::with_capacity(7);
    inner.push(Value::Str(spare_text("element")));
    let inner_pointer = inner.as_ptr();
    let inner_capacity = inner.capacity();
    let mut outer = Vec::with_capacity(9);
    outer.push(Value::Array(
        ArrayValue::with_lower_bounds(inner, vec![-10]).unwrap(),
    ));
    let outer_pointer = outer.as_ptr();
    let decoded = value_from_tagged_map_budgeted(
        retained_map(
            tagged(
                "array",
                [
                    ("values", Value::List(outer)),
                    (
                        "lower_bounds",
                        Value::List(vec![Value::Int(-3), Value::Int(4)]),
                    ),
                ],
            ),
            &memory,
        ),
        &CancellationToken::new(),
    )
    .unwrap();
    let Value::Array(array) = &*decoded else {
        panic!("expected an array")
    };
    assert_eq!(array.elements().as_ptr(), outer_pointer);
    assert_eq!(array.dimensions(), &[1, 1]);
    assert_eq!(array.lower_bounds(), &[-3, 4]);
    let Value::List(inner) = &array.elements()[0] else {
        panic!("expected normalized inner array")
    };
    assert_eq!(inner.as_ptr(), inner_pointer);
    assert_eq!(inner.capacity(), inner_capacity);
    assert_eq!(decoded.reserved_bytes(), payload(&decoded));
    drop(decoded);
    assert_eq!(memory.used(), 0);
}

#[test]
fn new_tag_allocations_fail_before_growth_and_preserve_other_result_leases() {
    let cases = [
        tagged("bytes", [("hex", Value::Str("0011223344556677".into()))]),
        tagged("decimal", [("value", Value::Str("1e4096".into()))]),
        tagged(
            "record",
            [(
                "fields",
                Value::List(vec![Value::List(vec![
                    Value::Str("x".into()),
                    Value::Int(1),
                ])]),
            )],
        ),
        tagged(
            "array",
            [
                ("values", Value::List(vec![])),
                ("lower_bounds", Value::List(vec![])),
            ],
        ),
    ];
    for map in cases {
        let value = Value::Map(map);
        let input_bytes = payload(&value);
        let memory = MemoryBudget::new(input_bytes + 17);
        let previous = Budgeted::new(vec![7_u8; 17], memory.reserve(17).unwrap());
        let Value::Map(map) = value else {
            unreachable!()
        };
        assert!(matches!(
            value_from_tagged_map_budgeted(retained_map(map, &memory), &CancellationToken::new()),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(memory.peak(), input_bytes + previous.reserved_bytes());
        assert_eq!(memory.used(), previous.reserved_bytes());
        assert_eq!(&**previous, &[7; 17]);
        drop(previous);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn invalid_tags_keep_original_payloads_and_cancellation_releases_only_the_input() {
    let inner = ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![-8]).unwrap();
    let pointer = inner.elements().as_ptr();
    let map = tagged(
        "array",
        [
            ("values", Value::List(vec![Value::Array(inner)])),
            ("lower_bounds", Value::List(vec![Value::Int(1)])),
        ],
    );
    let memory = MemoryBudget::new(1 << 20);
    let decoded =
        value_from_tagged_map_budgeted(retained_map(map, &memory), &CancellationToken::new())
            .unwrap();
    let Value::Map(map) = &*decoded else {
        panic!("invalid dimensions must retain the original map")
    };
    let Value::List(values) = &map["values"] else {
        panic!("expected original values")
    };
    let Value::Array(inner) = &values[0] else {
        panic!("invalid dimensions must not normalize nested arrays")
    };
    assert_eq!(inner.elements().as_ptr(), pointer);
    assert_eq!(inner.lower_bounds(), &[-8]);
    assert_eq!(decoded.reserved_bytes(), payload(&decoded));
    let previous = memory.used();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let input = retained_map(
        tagged("bytes", [("hex", Value::Str("00ff".into()))]),
        &memory,
    );
    assert!(matches!(
        value_from_tagged_map_budgeted(input, &cancellation),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(memory.used(), previous);
    assert_eq!(inner.elements().as_ptr(), pointer);
    drop(decoded);
    assert_eq!(memory.used(), 0);
}
