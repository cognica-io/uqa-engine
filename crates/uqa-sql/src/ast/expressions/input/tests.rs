//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod compatibility;
mod retention;

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<Expr>(text);
    let memory = MemoryBudget::new(1 << 26);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let actual = Expr::from_stored_schema_json(text, &memory, &original, &invoking);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?} / {expected:?}"
    );
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
        assert_eq!(actual.reserved_bytes(), memory.used());
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

const CALL: &str = r#"{"Func":{"name":"unresolved_native_call","binding":{"name":"selected_call","argument_types":["integer"],"dispatch":{"TypeOf":{"type_oid":23}},"composite_field":{"type_oid":40000,"number":2,"result_type":"Text","dropped":false}},"args":[{"Column":"x"},{"Literal":17}],"distinct":true,"order_by":[{"expr":{"Column":"y"},"descending":true,"nulls":"First"}],"order_syntax":"WithinGroup","filter":{"Literal":true}}}"#;

const ROW: &str = r#"{"CompositeRow":{"items":[{"Literal":1},{"Column":"y"}],"binding":{"ty":"schema.row","attributes":[2,4],"layout":{"width":5,"implicit":true},"argument_types":["Integer",{"Array":{"Domain":{"schema":"s","name":"d","oid":40000,"array_oid":40001,"base":"Text"}}}]}}}"#;

const TYPED: &str = r#"{"TypedLiteral":{"value":{"kept":[1,2,"a"]},"ty":"record","collation_oid":100,"composite_source":{"value":{"original":"retained"},"descriptors":[{"dropped":[{"number":2,"length":-1,"alignment":100}],"type_oid":40000,"relation_oid":50000,"attributes":[{"name":"a","ty":{"Array":"Text"},"collation_oid":100,"number":1}]}]}}}"#;

const STRUCTS: &[&str] = &[
    r#"{"QualifiedColumn":{"qualifier":"s.t","column":"a"}}"#,
    r#"{"InternalColumn":{"relation":18446744073709551615,"attribute":4294967295}}"#,
    r#"{"Binary":{"op":"Add","lhs":{"Literal":2},"rhs":{"Literal":3}}}"#,
    r#"{"IsNull":{"expr":{"Column":"x"},"negated":true}}"#,
    r#"{"Between":{"expr":{"Column":"x"},"low":{"Literal":1},"high":{"Literal":9}}}"#,
    r#"{"InList":{"expr":{"Column":"x"},"list":[{"Literal":1},{"Literal":9}],"negated":true}}"#,
    r#"{"Case":{"base":{"Column":"x"},"when":[[{"Literal":1},{"Literal":"one"}],[{"Literal":2},{"Literal":"two"}]],"else_branch":{"Literal":"other"}}}"#,
    r#"{"Cast":{"implicit":true,"expr":{"Column":"x"},"ty":"integer[]"}}"#,
    CALL,
    ROW,
    TYPED,
];

#[test]
fn stored_schema_input_preserves_all_scalar_nodes_and_child_positions() {
    for unit in ["Star", "Default"] {
        assert!(compare(&format!("{unit:?}")));
        assert!(compare(&format!(r#"{{"{unit}":null}}"#)));
    }
    for leaf in [
        r#"{"Column":"x"}"#,
        r#"{"QualifiedStar":"s.t"}"#,
        r#"{"Param":1}"#,
        r#"{"Literal":null}"#,
    ] {
        assert!(compare(leaf));
        for tag in ["Not", "UnaryMinus"] {
            assert!(compare(&format!(r#"{{"{tag}":{leaf}}}"#)));
        }
    }
    for tag in ["Array", "Row", "And", "Or"] {
        assert!(compare(&format!(r#"{{"{tag}":[]}}"#)));
        assert!(compare(&format!(r#"{{"{tag}":[{CALL},{ROW},{TYPED}]}}"#)));
    }
    for text in STRUCTS {
        assert!(compare(text));
    }
}

#[test]
fn stored_schema_input_reuses_native_constant_representation() {
    use std::collections::BTreeMap;
    for value in [
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(1.25),
        Value::Str("한글\0\"".into()),
        Value::FixedChar("a  ".into()),
        Value::Bytes(vec![0, 255, 2]),
        Value::Json(" {\"x\": 1, \"x\":2} ".into()),
        Value::JsonB("{\"a\":true}".into()),
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::Map(BTreeMap::from([("x".into(), Value::Int(2))])),
    ] {
        let encoded = serde_json::to_string(&Expr::Literal(value)).unwrap();
        assert!(compare(&encoded));
    }
    for text in [
        r#"{"Literal":1e9999}"#,
        r#"{"Literal":0.00000000000000000000000000000000000001}"#,
        r#"{"TypedLiteral":[1,"integer"]}"#,
    ] {
        assert!(compare(text));
    }
}

#[test]
fn stored_schema_input_rejects_query_and_window_forms_without_execution() {
    use crate::ast::Statement;
    for sql in [
        "SELECT (SELECT 1)",
        "SELECT EXISTS (SELECT 1)",
        "SELECT 1 IN (SELECT 1)",
        "SELECT sum(1) OVER ()",
    ] {
        let Statement::Select(select) = crate::compiler::compile(sql).unwrap().pop().unwrap()
        else {
            panic!("SELECT fixture")
        };
        let text = serde_json::to_string(&select.projections[0].expr).unwrap();
        assert!(serde_json::from_str::<Expr>(&text).is_ok());
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        assert!(Expr::from_stored_schema_json(&text, &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}
