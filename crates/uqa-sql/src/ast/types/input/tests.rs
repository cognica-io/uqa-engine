//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod compatibility;
mod retention;

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<ColumnType>(text);
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = ColumnType::from_stored_json_with_control(text, &control);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?} / {expected:?}"
    );
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        // Reference equality alone deliberately ignores names and non-identity metadata.
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
        assert_eq!(memory.used(), actual.reserved_bytes());
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

const STRUCTS: &[(&str, &str, &[&str])] = &[
    (
        "Numeric",
        r#"{"precision":20,"scale":-3}"#,
        &["precision", "scale"],
    ),
    (
        "IntervalWithFields",
        r#"{"fields":"DayToSecond","precision":4}"#,
        &["fields", "precision"],
    ),
    (
        "Domain",
        r#"{"schema":"a","name":"b","oid":40000,"array_oid":40001,"collation_oid":100,"base":{"Array":"Text"}}"#,
        &[
            "schema",
            "name",
            "oid",
            "array_oid",
            "collation_oid",
            "base",
        ],
    ),
    (
        "Enum",
        r#"{"schema":"a","name":"b","oid":40000,"array_oid":40001}"#,
        &["schema", "name", "oid", "array_oid"],
    ),
    (
        "Composite",
        r#"{"schema":"a","name":"b","oid":40000,"array_oid":40001,"relation_oid":50000}"#,
        &["schema", "name", "oid", "array_oid", "relation_oid"],
    ),
    (
        "DeclaredRange",
        r#"{"schema":"a","name":"b","oid":40000,"array_oid":40001,"alignment":"Double"}"#,
        &["schema", "name", "oid", "array_oid", "alignment"],
    ),
    (
        "DeclaredMultirange",
        r#"{"schema":"a","name":"b","oid":40000,"array_oid":40001,"alignment":"Integer"}"#,
        &["schema", "name", "oid", "array_oid", "alignment"],
    ),
];

fn tagged(tag: &str, payload: &str) -> String {
    format!("{{{tag:?}:{payload}}}")
}

const UNIT_TAGS: &[&str] = &[
    "SmallInteger",
    "Integer",
    "BigInteger",
    "Oid",
    "Xid",
    "Boolean",
    "Void",
    "Text",
    "RefCursor",
    "Name",
    "Uuid",
    "Bpchar",
    "Real",
    "DoublePrecision",
    "Json",
    "Xml",
    "JsonB",
    "Bytea",
    "InternalChar",
    "Regproc",
    "Regprocedure",
    "Regclass",
    "Regnamespace",
    "Regcollation",
    "Regrole",
    "Regtype",
    "PgNodeTree",
    "AclItem",
    "Int2Vector",
    "OidVector",
    "AnyArray",
    "Record",
    "Date",
    "Time",
    "TimeTz",
    "Timestamp",
    "TimestampTz",
    "Interval",
];

#[test]
fn stored_type_input_preserves_every_variant_and_owned_identity_field() {
    for tag in UNIT_TAGS {
        assert!(compare(&format!("{tag:?}")));
        assert!(compare(&tagged(tag, "null")));
        assert!(!compare(&tagged(tag, "0")));
    }
    for tag in ["Varchar", "Bit", "VarBit"] {
        assert!(compare(&tagged(tag, "null")));
        assert!(compare(&tagged(tag, "12")));
    }
    for tag in [
        "Character",
        "TimePrecision",
        "TimeTzPrecision",
        "TimestampPrecision",
        "TimestampTzPrecision",
        "Vector",
        "Tensor",
    ] {
        assert!(compare(&tagged(tag, "7")));
    }
    for payload in [r#""ordinary""#, r#""한글\u0000\ud83d\ude03\\\"""#] {
        assert!(compare(&tagged("Named", payload)));
    }
    for tag in ["Range", "Multirange"] {
        for subtype in [
            "Integer",
            "BigInteger",
            "Numeric",
            "Date",
            "Timestamp",
            "TimestampTz",
        ] {
            assert!(compare(&tagged(tag, &format!("{subtype:?}"))));
            assert!(compare(&tagged(tag, &tagged(subtype, "null"))));
        }
    }
    for &(tag, payload, _) in STRUCTS {
        let text = tagged(tag, payload);
        assert!(compare(&text));
        assert!(compare(&tagged("Array", &text)));
    }
    for fields in [
        "All",
        "Year",
        "Month",
        "Day",
        "Hour",
        "Minute",
        "Second",
        "YearToMonth",
        "DayToHour",
        "DayToMinute",
        "DayToSecond",
        "HourToMinute",
        "HourToSecond",
        "MinuteToSecond",
    ] {
        assert!(compare(&tagged(
            "IntervalWithFields",
            &format!("{{\"fields\":{fields:?}}}")
        )));
        assert!(compare(&tagged(
            "IntervalWithFields",
            &format!("[{{{fields:?}:null}},null]")
        )));
    }
}
