//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 overload selection for the regular-expression functions, as independently captured in `tests/parity/pg18/regular_expression_identities_oracle.expected.json`.

use super::super::*;

fn select(
    name: &str,
    names: &[Option<&str>],
    types: &[Option<ColumnType>],
) -> Result<ResolvedFixedBuiltinCall, SQLError> {
    let names = names
        .iter()
        .map(|name| name.map(str::to_owned))
        .collect::<Vec<_>>();
    resolve_fixed_builtin_call(name, None, &names, types, false, None)
        .map(|resolved| resolved.expect("regular-expression functions are fixed built-ins"))
}

fn positional(name: &str, types: &[Option<ColumnType>]) -> ResolvedFixedBuiltinCall {
    select(name, &vec![None; types.len()], types)
        .unwrap_or_else(|error| panic!("{name}{types:?}: {error}"))
}

fn argument_names(resolved: &ResolvedFixedBuiltinCall) -> Vec<&str> {
    resolved
        .selected
        .binding
        .argument_types
        .iter()
        .map(String::as_str)
        .collect()
}

#[test]
fn every_regular_expression_overload_selects_its_own_declaration() {
    use ColumnType::{Boolean, Integer, Text};
    let text_array = || ColumnType::Array(Box::new(Text));
    let cases: Vec<(&str, Vec<ColumnType>, ColumnType)> = vec![
        ("regexp_count", vec![Text, Text], Integer),
        ("regexp_count", vec![Text, Text, Integer], Integer),
        ("regexp_count", vec![Text, Text, Integer, Text], Integer),
        ("regexp_instr", vec![Text, Text], Integer),
        ("regexp_instr", vec![Text, Text, Integer], Integer),
        ("regexp_instr", vec![Text, Text, Integer, Integer], Integer),
        (
            "regexp_instr",
            vec![Text, Text, Integer, Integer, Integer],
            Integer,
        ),
        (
            "regexp_instr",
            vec![Text, Text, Integer, Integer, Integer, Text],
            Integer,
        ),
        (
            "regexp_instr",
            vec![Text, Text, Integer, Integer, Integer, Text, Integer],
            Integer,
        ),
        ("regexp_like", vec![Text, Text], Boolean),
        ("regexp_like", vec![Text, Text, Text], Boolean),
        ("regexp_match", vec![Text, Text], text_array()),
        ("regexp_match", vec![Text, Text, Text], text_array()),
        ("regexp_matches", vec![Text, Text], text_array()),
        ("regexp_matches", vec![Text, Text, Text], text_array()),
        ("regexp_replace", vec![Text, Text, Text], Text),
        ("regexp_replace", vec![Text, Text, Text, Text], Text),
        ("regexp_replace", vec![Text, Text, Text, Integer], Text),
        (
            "regexp_replace",
            vec![Text, Text, Text, Integer, Integer],
            Text,
        ),
        (
            "regexp_replace",
            vec![Text, Text, Text, Integer, Integer, Text],
            Text,
        ),
        ("regexp_split_to_array", vec![Text, Text], text_array()),
        (
            "regexp_split_to_array",
            vec![Text, Text, Text],
            text_array(),
        ),
        ("regexp_split_to_table", vec![Text, Text], Text),
        ("regexp_split_to_table", vec![Text, Text, Text], Text),
        ("regexp_substr", vec![Text, Text], Text),
        ("regexp_substr", vec![Text, Text, Integer], Text),
        ("regexp_substr", vec![Text, Text, Integer, Integer], Text),
        (
            "regexp_substr",
            vec![Text, Text, Integer, Integer, Text],
            Text,
        ),
        (
            "regexp_substr",
            vec![Text, Text, Integer, Integer, Text, Integer],
            Text,
        ),
    ];
    assert_eq!(cases.len(), 29);
    for (name, arguments, result) in cases {
        let types = arguments.iter().cloned().map(Some).collect::<Vec<_>>();
        let resolved = positional(&format!("PG_CATALOG.{}", name.to_uppercase()), &types);
        assert_eq!(resolved.selected.binding.name, format!("pg_catalog.{name}"));
        assert!(resolved.selected.binding.builtin);
        assert_eq!(resolved.selected.binding.dispatch, None, "{name}");
        assert_eq!(resolved.selected.return_type, result, "{name}{arguments:?}");
        assert_eq!(
            argument_names(&resolved),
            arguments
                .iter()
                .map(ColumnType::sql_name)
                .collect::<Vec<_>>(),
            "{name}"
        );
        assert!(!resolved.builtin_non_immutable, "{name}");
    }
}

#[test]
fn unknown_and_coercible_arguments_select_postgresql_candidates() {
    let unknown = |count| vec![None; count];
    // `regexp_replace(unknown, unknown, unknown, unknown)` prefers the string category, so `flags` wins over `start`.
    assert_eq!(
        argument_names(&positional("regexp_replace", &unknown(4))),
        ["text", "text", "text", "text"]
    );
    let integer_start = [None, None, None, Some(ColumnType::Integer)];
    assert_eq!(
        argument_names(&positional("regexp_replace", &integer_start)),
        ["text", "text", "text", "integer"]
    );
    // Only one three-argument `regexp_count` exists, so an unknown third argument is read as its integer `start`.
    assert_eq!(
        argument_names(&positional("regexp_count", &unknown(3))),
        ["text", "text", "integer"]
    );
    let smallint_start = [None, None, Some(ColumnType::SmallInteger)];
    assert_eq!(
        argument_names(&positional("regexp_count", &smallint_start)),
        ["text", "text", "integer"]
    );
    for input in [
        ColumnType::Varchar(None),
        ColumnType::Bpchar,
        ColumnType::Name,
    ] {
        let types = [Some(input.clone()), None];
        assert_eq!(
            argument_names(&positional("regexp_like", &types)),
            ["text", "text"],
            "{input:?}"
        );
    }
}

#[test]
fn named_arguments_use_declared_names_and_exact_case() {
    let resolved = select(
        "regexp_instr",
        &[None, None, None, Some("N")],
        &[
            None,
            None,
            Some(ColumnType::Integer),
            Some(ColumnType::Integer),
        ],
    )
    .unwrap();
    assert_eq!(
        argument_names(&resolved),
        ["text", "text", "integer", "integer"]
    );
    assert_eq!(resolved.builtin_argument_positions, Some(vec![0, 1, 2, 3]));
    let reordered = select(
        "regexp_substr",
        &[
            Some("subexpr"),
            Some("flags"),
            Some("N"),
            Some("start"),
            Some("pattern"),
            Some("string"),
        ],
        &[
            Some(ColumnType::Integer),
            None,
            Some(ColumnType::Integer),
            Some(ColumnType::Integer),
            None,
            None,
        ],
    )
    .unwrap();
    assert_eq!(
        argument_names(&reordered),
        ["text", "text", "integer", "integer", "text", "integer"]
    );
    assert_eq!(
        reordered.builtin_argument_positions,
        Some(vec![5, 4, 3, 2, 1, 0])
    );
    let start = select(
        "regexp_replace",
        &[None, None, None, Some("start")],
        &[None, None, None, Some(ColumnType::Integer)],
    )
    .unwrap();
    assert_eq!(argument_names(&start), ["text", "text", "text", "integer"]);
    let flags = select(
        "regexp_replace",
        &[None, None, None, Some("flags")],
        &[None, None, None, None],
    )
    .unwrap();
    assert_eq!(argument_names(&flags), ["text", "text", "text", "text"]);
    let lowercase = select(
        "regexp_instr",
        &[None, None, Some("n")],
        &[None, None, Some(ColumnType::Integer)],
    )
    .unwrap_err();
    assert_eq!(lowercase.sqlstate(), Some("42883"));
    assert_eq!(
        lowercase.to_string(),
        "function regexp_instr(unknown, unknown, n => integer) does not exist"
    );
    let duplicate = select(
        "regexp_like",
        &[Some("string"), Some("string")],
        &[None, None],
    )
    .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42601"));
    assert_eq!(
        duplicate.to_string(),
        "argument name \"string\" used more than once"
    );
}

#[test]
fn incompatible_calls_report_postgresql_resolution_errors() {
    for (name, names, types, message) in [
        (
            "regexp_matches",
            vec![None, None],
            vec![Some(ColumnType::Integer), None],
            "function regexp_matches(integer, unknown) does not exist",
        ),
        (
            "regexp_matches",
            vec![Some("string"), Some("pattern")],
            vec![Some(ColumnType::Integer), None],
            "function regexp_matches(string => integer, pattern => unknown) does not exist",
        ),
        (
            "regexp_matches",
            vec![None],
            vec![None],
            "function regexp_matches(unknown) does not exist",
        ),
        (
            "regexp_matches",
            vec![None; 4],
            vec![None; 4],
            "function regexp_matches(unknown, unknown, unknown, unknown) does not exist",
        ),
        (
            "regexp_split_to_table",
            vec![None, None],
            vec![Some(ColumnType::Integer), None],
            "function regexp_split_to_table(integer, unknown) does not exist",
        ),
        (
            "regexp_replace",
            vec![None; 4],
            vec![None, None, None, Some(ColumnType::BigInteger)],
            "function regexp_replace(unknown, unknown, unknown, bigint) does not exist",
        ),
        (
            "regexp_like",
            vec![None; 3],
            vec![None, None, Some(ColumnType::Integer)],
            "function regexp_like(unknown, unknown, integer) does not exist",
        ),
        (
            "regexp_like",
            vec![None, Some("pattern"), Some("options")],
            vec![None, None, None],
            "function regexp_like(unknown, pattern => unknown, options => unknown) does not exist",
        ),
    ] {
        let error = select(name, &names, &types).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{message}");
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn untyped_parameters_take_the_selected_declared_types() {
    let text = || SQLParam::scalar(Value::Str("a".into()));
    let integer = || SQLParam::scalar(Value::Int(2));
    for (function, parameters, expected) in [
        ("regexp_matches", vec![text(), text()], vec!["text", "text"]),
        (
            "regexp_split_to_table",
            vec![text(), text(), text()],
            vec!["text", "text", "text"],
        ),
        (
            "regexp_count",
            vec![text(), text(), integer()],
            vec!["text", "text", "integer"],
        ),
        (
            "regexp_replace",
            vec![text(), text(), text(), text()],
            vec!["text", "text", "text", "text"],
        ),
        (
            "regexp_replace",
            vec![text(), text(), text(), integer()],
            vec!["text", "text", "text", "integer"],
        ),
    ] {
        let expression = ScalarExpr::Func {
            order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
            name: function.into(),
            binding: None,
            args: (1..=parameters.len()).map(ScalarExpr::Param).collect(),
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let ScalarExpr::Func { binding, args, .. } =
            bind_type_introspection(expression, &RowSchema::default(), &parameters)
        else {
            panic!("{function} must remain a call");
        };
        let binding = binding.unwrap_or_else(|| panic!("{function} must be bound"));
        assert_eq!(binding.name, format!("pg_catalog.{function}"));
        assert_eq!(binding.argument_types, expected, "{function}");
        for (position, (argument, expected)) in args.iter().zip(&expected).enumerate() {
            let declared = ColumnType::from_sql_name(expected).unwrap().catalog_name();
            assert!(
                matches!(
                    argument,
                    ScalarExpr::Cast { expr, ty, .. }
                        if *ty == declared
                            && matches!(expr.as_ref(), ScalarExpr::Param(index) if *index == position + 1)
                ),
                "{function} argument {position}: {argument:?}"
            );
        }
    }
}
