//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn tagged(
    tag: &str,
    fields: impl IntoIterator<Item = (&'static str, Value)>,
) -> BTreeMap<String, Value> {
    std::iter::once(("$uqa_type".to_owned(), Value::Str(tag.to_owned())))
        .chain(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value)),
        )
        .collect()
}

fn spare_text(text: &str) -> String {
    let mut value = String::with_capacity(text.len() + 128);
    value.push_str(text);
    value
}

#[test]
fn text_tags_move_the_original_string_buffer() {
    for tag in ["fixed_char", "json", "jsonb"] {
        let text = spare_text("payload");
        let pointer = text.as_ptr();
        let capacity = text.capacity();
        let decoded = value_from_tagged_map(tagged(tag, [("value", Value::Str(text))])).unwrap();
        let (("fixed_char", Value::FixedChar(text))
        | ("json", Value::Json(text))
        | ("jsonb", Value::JsonB(text))) = (tag, decoded)
        else {
            panic!("decoded the wrong value variant");
        };
        assert_eq!(text, "payload");
        assert_eq!(text.as_ptr(), pointer);
        assert_eq!(text.capacity(), capacity);
    }
}

#[test]
fn rows_move_the_original_vector_and_nested_payloads() {
    let text = spare_text("retained");
    let text_pointer = text.as_ptr();
    let mut values = Vec::with_capacity(12);
    values.extend([Value::Str(text), Value::Null]);
    let pointer = values.as_ptr();
    let capacity = values.capacity();
    let Value::Row(values) =
        value_from_tagged_map(tagged("row", [("values", Value::List(values))])).unwrap()
    else {
        panic!("expected a row");
    };
    assert_eq!(values.as_ptr(), pointer);
    assert_eq!(values.capacity(), capacity);
    let Value::Str(text) = &values[0] else {
        panic!("expected text")
    };
    assert_eq!(text.as_ptr(), text_pointer);
    assert_eq!(values[1], Value::Null);
}

#[test]
fn records_preserve_field_order_duplicate_names_and_payload_ownership() {
    let name = spare_text("same");
    let name_pointer = name.as_ptr();
    let text = spare_text("nested");
    let text_pointer = text.as_ptr();
    let bytes = vec![0, 127, 255];
    let bytes_pointer = bytes.as_ptr();
    let fields = vec![
        Value::List(vec![Value::Str(name), Value::List(vec![Value::Str(text)])]),
        Value::List(vec![Value::Str("same".into()), Value::Bytes(bytes)]),
    ];
    let Value::Record(fields) =
        value_from_tagged_map(tagged("record", [("fields", Value::List(fields))])).unwrap()
    else {
        panic!("expected a record");
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].0, "same");
    assert_eq!(fields[1].0, "same");
    assert_eq!(fields[0].0.as_ptr(), name_pointer);
    let Value::List(values) = &fields[0].1 else {
        panic!("expected nested values")
    };
    let Value::Str(text) = &values[0] else {
        panic!("expected text")
    };
    assert_eq!(text.as_ptr(), text_pointer);
    let Value::Bytes(bytes) = &fields[1].1 else {
        panic!("expected bytes")
    };
    assert_eq!(bytes.as_ptr(), bytes_pointer);
}

#[test]
fn arrays_normalize_nested_arrays_without_copying_their_buffers() {
    let text = spare_text("element");
    let text_pointer = text.as_ptr();
    let mut inner = Vec::with_capacity(7);
    inner.push(Value::Str(text));
    let inner_pointer = inner.as_ptr();
    let inner_capacity = inner.capacity();
    let inner = ArrayValue::with_lower_bounds(inner, vec![-10]).unwrap();
    let mut outer = Vec::with_capacity(9);
    outer.push(Value::Array(inner));
    let outer_pointer = outer.as_ptr();
    let outer_capacity = outer.capacity();
    let Value::Array(array) = value_from_tagged_map(tagged(
        "array",
        [
            ("values", Value::List(outer)),
            (
                "lower_bounds",
                Value::List(vec![Value::Int(-3), Value::Int(4)]),
            ),
        ],
    ))
    .unwrap() else {
        panic!("expected an array")
    };
    assert_eq!(array.dimensions(), &[1, 1]);
    assert_eq!(array.lower_bounds(), &[-3, 4]);
    let outer = array.into_elements();
    assert_eq!(outer.as_ptr(), outer_pointer);
    assert_eq!(outer.capacity(), outer_capacity);
    let Value::List(inner) = &outer[0] else {
        panic!("expected normalized inner array")
    };
    assert_eq!(inner.as_ptr(), inner_pointer);
    assert_eq!(inner.capacity(), inner_capacity);
    let Value::Str(text) = &inner[0] else {
        panic!("expected text")
    };
    assert_eq!(text.as_ptr(), text_pointer);
}

#[test]
fn invalid_composite_tags_preserve_the_original_map() {
    let cases = [
        tagged(
            "array",
            [
                ("values", Value::List(vec![Value::Int(1)])),
                ("lower_bounds", Value::List(Vec::new())),
            ],
        ),
        tagged(
            "array",
            [
                (
                    "values",
                    Value::List(vec![Value::List(vec![Value::Int(1)]), Value::Int(2)]),
                ),
                (
                    "lower_bounds",
                    Value::List(vec![Value::Int(1), Value::Int(1)]),
                ),
            ],
        ),
        tagged(
            "array",
            [
                ("values", Value::List(vec![Value::Int(1)])),
                ("lower_bounds", Value::List(vec![Value::Int(i64::MAX)])),
            ],
        ),
        tagged(
            "record",
            [(
                "fields",
                Value::List(vec![
                    Value::List(vec![Value::Str("valid".into()), Value::Int(1)]),
                    Value::List(vec![Value::Int(2), Value::Int(3)]),
                ]),
            )],
        ),
        tagged("row", [("values", Value::Int(1))]),
    ];
    for map in cases {
        let original = map.clone();
        let tag_pointer = match &map["$uqa_type"] {
            Value::Str(tag) => tag.as_ptr(),
            _ => unreachable!(),
        };
        let Value::Map(decoded) = value_from_tagged_map(map).unwrap() else {
            panic!("invalid tag must remain a map")
        };
        assert_eq!(decoded, original);
        let Value::Str(tag) = &decoded["$uqa_type"] else {
            panic!("expected original tag")
        };
        assert_eq!(tag.as_ptr(), tag_pointer);
    }
}

#[test]
fn nested_tagged_documents_round_trip_with_bounds_names_and_bytes() {
    let value = Value::Record(vec![
        (
            "mixed".into(),
            Value::Row(vec![
                Value::FixedChar("a  ".into()),
                Value::Json(" {\"a\": 1} ".into()),
                Value::JsonB("{\"a\":1}".into()),
                Value::Bytes(vec![0, 255]),
            ]),
        ),
        (
            "mixed".into(),
            Value::Array(
                ArrayValue::with_lower_bounds(
                    vec![Value::List(vec![Value::Int(1), Value::Null])],
                    vec![-2, 4],
                )
                .unwrap(),
            ),
        ),
    ]);
    let encoded = serde_json::to_vec(&value).unwrap();
    let decoded: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
}

#[test]
fn empty_dimensions_and_mixed_nested_empty_arrays_keep_their_shape() {
    let empty: Value =
        serde_json::from_str(r#"{"$uqa_type":"array","values":[],"lower_bounds":[]}"#).unwrap();
    let Value::Array(empty) = empty else {
        panic!("expected an empty array")
    };
    assert!(empty.dimensions().is_empty());
    assert!(empty.lower_bounds().is_empty());

    let nested = ArrayValue::with_lower_bounds(
        vec![Value::Array(empty), Value::List(Vec::new())],
        vec![-2, 7],
    )
    .unwrap();
    assert_eq!(nested.dimensions(), &[2, 0]);
    assert_eq!(nested.lower_bounds(), &[-2, 7]);
    assert_eq!(
        nested.elements(),
        &[Value::List(Vec::new()), Value::List(Vec::new())]
    );
    let encoded = serde_json::to_vec(&Value::Array(nested.clone())).unwrap();
    let decoded: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, Value::Array(nested));
}

#[test]
fn rejected_array_tags_do_not_normalize_the_original_nested_values() {
    let inner =
        ArrayValue::with_lower_bounds(vec![Value::Str(spare_text("kept"))], vec![-8]).unwrap();
    let pointer = inner.elements().as_ptr();
    let map = tagged(
        "array",
        [
            ("values", Value::List(vec![Value::Array(inner)])),
            ("lower_bounds", Value::List(vec![Value::Int(1)])),
        ],
    );
    let Value::Map(map) = value_from_tagged_map(map).unwrap() else {
        panic!("invalid shape must remain a map")
    };
    let Value::List(values) = &map["values"] else {
        panic!("expected original values")
    };
    let Value::Array(inner) = &values[0] else {
        panic!("nested array must remain typed")
    };
    assert_eq!(inner.lower_bounds(), &[-8]);
    assert_eq!(inner.elements().as_ptr(), pointer);
}

#[test]
fn array_normalization_preserves_arrays_inside_composite_values() {
    let inner = Value::Array(ArrayValue::with_lower_bounds(vec![Value::Int(7)], vec![-5]).unwrap());
    let composites = vec![
        Value::Row(vec![inner.clone()]),
        Value::Record(vec![("array".into(), inner.clone())]),
        Value::Map(BTreeMap::from([("array".into(), inner)])),
    ];
    let array = ArrayValue::with_lower_bounds(composites.clone(), vec![3]).unwrap();
    assert_eq!(array.dimensions(), &[3]);
    assert_eq!(array.elements(), composites);
    let encoded = serde_json::to_vec(&Value::Array(array.clone())).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&encoded).unwrap(),
        Value::Array(array)
    );
}
