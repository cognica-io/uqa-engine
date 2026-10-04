//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{flatten_set_arguments, SetArgument};

fn text(value: &str) -> SetArgument {
    SetArgument::Text(value.into())
}

#[test]
fn list_parameters_join_their_arguments_and_quote_identifiers() {
    assert_eq!(
        flatten_set_arguments(
            "search_path",
            &[text("a$b"), text("public"), text("My S"), text("$user")]
        )
        .unwrap(),
        "\"a$b\", public, \"My S\", \"$user\""
    );
    assert_eq!(
        flatten_set_arguments("search_path", &[text("")]).unwrap(),
        "\"\""
    );
    assert_eq!(
        flatten_set_arguments("search_path", &[text("a,b")]).unwrap(),
        "\"a,b\""
    );
    assert_eq!(
        flatten_set_arguments("DateStyle", &[text("ISO"), text("DMY")]).unwrap(),
        "ISO, DMY"
    );
}

#[test]
fn other_parameters_take_one_argument_as_written() {
    assert_eq!(
        flatten_set_arguments("statement_timeout", &[SetArgument::Number("1.5".into())]).unwrap(),
        "1.5"
    );
    assert_eq!(
        flatten_set_arguments("work_mem", &[SetArgument::Integer(65536)]).unwrap(),
        "65536"
    );
    assert_eq!(
        flatten_set_arguments("my.var", &[text("My Value")]).unwrap(),
        "My Value"
    );
    let error = flatten_set_arguments(
        "statement_timeout",
        &[SetArgument::Integer(1), SetArgument::Integer(2)],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(
        error.to_string(),
        "SET statement_timeout takes only one argument"
    );
}
