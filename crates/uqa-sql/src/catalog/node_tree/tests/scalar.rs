//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar catalog constants validate their owned physical bounds before output.

use super::{deparse, values, Field, Node, Routines};
use crate::ColumnType;
use uqa_core::Value;

fn replace(node: &mut Node, name: &str, value: Field) {
    node.fields
        .iter_mut()
        .find(|(key, _)| key == name)
        .unwrap()
        .1 = value;
}

#[test]
fn scalar_datums_reject_truncated_overlong_and_inconsistent_layouts() {
    for (ty, value) in [
        (ColumnType::Bytea, Value::Bytes(vec![0, 255])),
        (
            ColumnType::Uuid,
            Value::Str("00112233-4455-6677-8899-aabbccddeeff".into()),
        ),
        (ColumnType::Json, Value::Json(r#"{"x":1}"#.into())),
        (ColumnType::JsonB, Value::JsonB(r#"{"x":1}"#.into())),
    ] {
        let valid = values::constant(&value, &ty).unwrap();
        let Field::Datum { length, bytes } = valid.field("constvalue").unwrap() else {
            panic!("physical constant")
        };
        let mut truncated = bytes.clone();
        truncated.pop();
        let mut extended = bytes.clone();
        extended.push(0);
        for (field, value) in [
            ("constbyval", true.into()),
            ("constlen", 0.into()),
            (
                "constvalue",
                Field::Datum {
                    length: length - 1,
                    bytes: bytes.clone(),
                },
            ),
            (
                "constvalue",
                Field::Datum {
                    length: length - 1,
                    bytes: truncated,
                },
            ),
            (
                "constvalue",
                Field::Datum {
                    length: length + 1,
                    bytes: extended,
                },
            ),
        ] {
            let mut node = valid.clone();
            replace(&mut node, field, value);
            for pretty in [false, true] {
                assert!(
                    deparse::expression(&node.clone().into(), &Routines, pretty).is_err(),
                    "{ty:?} {field}"
                );
            }
        }
    }
}

#[test]
fn malformed_json_payloads_fail_and_null_constants_never_read_payloads() {
    for (ty, payload) in [
        (ColumnType::Json, vec![20, 0, 0, 0, 255]),
        (ColumnType::JsonB, vec![16, 0, 0, 0]),
    ] {
        let mut node = values::constant(&Value::Null, &ty).unwrap();
        replace(&mut node, "constisnull", false.into());
        replace(
            &mut node,
            "constvalue",
            Field::Datum {
                length: payload.len(),
                bytes: payload,
            },
        );
        assert!(deparse::expression(&node.into(), &Routines, false).is_err());
    }
    for ty in [
        ColumnType::Bytea,
        ColumnType::Uuid,
        ColumnType::Json,
        ColumnType::JsonB,
    ] {
        let mut node = values::constant(&Value::Null, &ty).unwrap();
        replace(
            &mut node,
            "constvalue",
            Field::Datum {
                length: usize::MAX,
                bytes: vec![],
            },
        );
        for pretty in [false, true] {
            assert_eq!(
                deparse::expression(&node.clone().into(), &Routines, pretty).unwrap(),
                format!("NULL::{}", ty.sql_name())
            );
        }
    }
}
