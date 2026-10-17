//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constructor shape and NULL diagnostics come from independent `PostgreSQL` calls.

use super::*;

#[test]
fn multirange_constructor_input_errors_preserve_postgresql_and_production_owners() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("constructor-inputs-postgresql.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 25);
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for (family, (name, input)) in [
        ("int4multirange", "[1,4)"),
        ("int8multirange", "[1,4)"),
        ("nummultirange", "[1,4)"),
        ("datemultirange", "[2000-01-01,2000-01-04)"),
        ("tsmultirange", "[2000-01-01,2000-01-04)"),
        ("tstzmultirange", "[2000-01-01,2000-01-04)"),
    ]
    .into_iter()
    .enumerate()
    {
        let range = Value::Str(input.into());
        let array = |values| Value::Array(ArrayValue::try_new(values).unwrap());
        for (position, arguments) in [
            vec![array(vec![range.clone(), Value::Null])],
            vec![array(vec![array(vec![range.clone()])])],
            vec![Value::Null, range],
        ]
        .into_iter()
        .enumerate()
        {
            let unchanged = arguments.clone();
            let expected = &cases[family * 3 + position]["error"];
            for error in [
                eval_range_functions(name, &arguments).unwrap().unwrap_err(),
                eval_range_functions_with_control(name, &arguments, &control)
                    .unwrap()
                    .unwrap_err(),
            ] {
                assert_eq!(
                    serde_json::json!({
                        "sqlstate": error.sqlstate(),
                        "message": error.to_string(),
                        "detail": error.detail(),
                        "hint": error.hint(),
                    }),
                    *expected,
                    "{name} {position}",
                );
                assert_eq!(arguments, unchanged);
                assert_eq!(memory.used(), 0);
            }
        }
    }
    let input = vec![Value::Array(
        ArrayValue::try_new(vec![Value::Str("[1,4)".into()), Value::Null]).unwrap(),
    )];
    let exhausted = MemoryBudget::new(0);
    let limited = ProductionControl::new(&exhausted, &original, &invoking);
    let error = eval_range_functions_with_control("int4multirange", &input, &limited)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(exhausted.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error = eval_range_functions_with_control("int4multirange", &input, &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
