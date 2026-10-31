//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    builtin_table_function_overloads, upgrade_legacy_table_function_binding,
    user_function_declared_columns, validate_table_function_column_definition,
    ColumnDefinitionRequest,
};
use crate::ast::{ColumnType, FunctionBinding};
use crate::routines::{RoutineBody, SQLUserFunction};
use crate::{RowSchema, SQLError, ScalarExpr};

fn legacy_binding(kind: &str) -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: "pg_catalog.generate_series".into(),
        argument_types: vec![kind.into(); 3],
        builtin: true,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }
}

#[test]
fn integer_series_keeps_distinct_two_and_three_argument_identities() {
    let overloads = builtin_table_function_overloads("generate_series", &[]);
    assert_eq!(overloads.len(), 4);
    for kind in [ColumnType::Integer, ColumnType::BigInteger] {
        for count in [2, 3] {
            let overload = overloads
                .iter()
                .find(|overload| overload.argument_types == vec![kind.clone(); count])
                .unwrap();
            assert_eq!(overload.default_arguments, 0);
            assert_eq!(overload.return_type, kind);
        }
    }
}

#[test]
fn legacy_series_upgrade_preserves_user_identities_and_explicit_steps() {
    for kind in ["integer", "bigint"] {
        let mut binding = Some(legacy_binding(kind));
        assert!(upgrade_legacy_table_function_binding(&mut binding, 2));
        assert_eq!(binding.as_ref().unwrap().argument_types, [kind, kind]);
        assert!(!upgrade_legacy_table_function_binding(&mut binding, 2));
        for count in [0, 1, 3, 4] {
            let mut unchanged = Some(legacy_binding(kind));
            let original = unchanged.clone();
            assert!(!upgrade_legacy_table_function_binding(
                &mut unchanged,
                count
            ));
            assert_eq!(unchanged, original);
        }
    }
    let mut user = legacy_binding("integer");
    user.builtin = false;
    user.object_id = Some([7; 16]);
    let mut other_schema = legacy_binding("integer");
    other_schema.name = "public.generate_series".into();
    let mut mixed = legacy_binding("integer");
    mixed.argument_types[2] = "bigint".into();
    for binding in [user, other_schema, mixed, legacy_binding("numeric")] {
        let mut unchanged = Some(binding);
        let original = unchanged.clone();
        assert!(!upgrade_legacy_table_function_binding(&mut unchanged, 2));
        assert_eq!(unchanged, original);
    }
}

#[test]
fn legacy_series_upgrade_reaches_stored_statement_and_view_sources() {
    let mut statement = crate::compile(
        "SELECT * FROM ROWS FROM(generate_series(1,3), generate_series(4,6)) AS s(a,b)",
    )
    .unwrap()
    .remove(0);
    let crate::ast::Statement::Select(select) = &mut statement else {
        panic!("query");
    };
    let Some(crate::ast::FromClause::FunctionGroup { functions, .. }) = &mut select.from else {
        panic!("range function group");
    };
    for function in functions {
        function.binding = Some(legacy_binding("integer"));
    }
    let mut plan = crate::plan::UnifiedPlan::lower(statement.clone());
    assert!(statement.upgrade_legacy_serialized_dispatches());
    assert!(!statement.upgrade_legacy_serialized_dispatches());
    let crate::plan::UnifiedPlan::Query(query) = &mut plan else {
        panic!("query");
    };
    assert!(crate::binding::view_dependencies::restoration::upgrade_legacy_view_dispatches(query));
    assert!(!crate::binding::view_dependencies::restoration::upgrade_legacy_view_dispatches(query));
}

#[test]
fn regular_expression_set_functions_share_the_scalar_catalog_signatures() {
    for (name, result) in [
        (
            "regexp_matches",
            ColumnType::Array(Box::new(ColumnType::Text)),
        ),
        ("regexp_split_to_table", ColumnType::Text),
    ] {
        let overloads = builtin_table_function_overloads(name, &[]);
        assert_eq!(
            Some(&overloads),
            crate::type_resolution::fixed_builtin_overloads(name).as_ref()
        );
        let declared = overloads
            .iter()
            .map(|overload| {
                (
                    overload.name.as_str(),
                    overload
                        .argument_names
                        .iter()
                        .map(|name| name.as_deref().unwrap())
                        .collect::<Vec<_>>(),
                    overload.argument_types.clone(),
                    overload.default_arguments,
                    overload.return_type.clone(),
                )
            })
            .collect::<Vec<_>>();
        let qualified = format!("pg_catalog.{name}");
        assert_eq!(
            declared,
            [
                (
                    qualified.as_str(),
                    vec!["string", "pattern"],
                    vec![ColumnType::Text; 2],
                    0,
                    result.clone(),
                ),
                (
                    qualified.as_str(),
                    vec!["string", "pattern", "flags"],
                    vec![ColumnType::Text; 3],
                    0,
                    result,
                ),
            ]
        );
    }
}

struct NoRoutines;

impl crate::FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl crate::routines::RoutineResolution for NoRoutines {}

struct BuiltinRoutines;

impl crate::FunctionTypeResolver for BuiltinRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl crate::routines::RoutineResolution for BuiltinRoutines {
    fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
        builtins: &[crate::type_resolution::BuiltinFunctionOverload],
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        assert!(!variadic);
        crate::type_resolution::resolve_local_builtin_overload(
            name, binding, names, types, builtins,
        )
        .map(Some)
    }
}

#[test]
fn dynamic_table_arguments_bind_from_values_without_retyping_declared_inputs() {
    use super::{resolve_runtime_table_function_binding, resolve_table_function_binding};
    use uqa_core::Value;
    let args = vec![ScalarExpr::qualified_column("local", "n"); 2];
    for (declared, expected) in [(None, "integer"), (Some(ColumnType::BigInteger), "bigint")] {
        let schema =
            RowSchema::with_qualified_types("local", vec!["n".into()], vec![declared.clone()]);
        let binding = resolve_table_function_binding(
            &BuiltinRoutines,
            "generate_series",
            None,
            &args,
            &schema,
            &[],
            &BuiltinRoutines,
        )
        .unwrap();
        assert_eq!(binding.is_some(), declared.is_some());
        let observed = resolve_runtime_table_function_binding(
            &BuiltinRoutines,
            "generate_series",
            &args,
            &[(None, Value::Int(1)), (None, Value::Int(2))],
            &schema,
            &[],
            &BuiltinRoutines,
        )
        .unwrap()
        .unwrap();
        assert_eq!(observed.argument_types, [expected, expected]);
    }
    let schema = RowSchema::default();
    let dynamic_schema = RowSchema::with_qualified_types("local", vec!["n".into()], vec![None]);
    let error = resolve_runtime_table_function_binding(
        &BuiltinRoutines,
        "generate_series",
        &args,
        &[
            (None, Value::Str("1".into())),
            (None, Value::Str("2".into())),
        ],
        &dynamic_schema,
        &[],
        &BuiltinRoutines,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    let literals = vec![ScalarExpr::Literal(Value::Null); 2];
    let error = resolve_table_function_binding(
        &BuiltinRoutines,
        "generate_series",
        None,
        &literals,
        &schema,
        &[],
        &BuiltinRoutines,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
    let literals = vec![
        ScalarExpr::Literal(Value::Str("1".into())),
        ScalarExpr::Literal(Value::Int(2)),
    ];
    let observed = resolve_runtime_table_function_binding(
        &BuiltinRoutines,
        "generate_series",
        &literals,
        &[(None, Value::Str("1".into())), (None, Value::Int(2))],
        &schema,
        &[],
        &BuiltinRoutines,
    )
    .unwrap()
    .unwrap();
    assert_eq!(observed.argument_types, ["integer", "integer"]);
}

fn column_definition(name: &str, args: &[ScalarExpr], declared: &[&str]) -> Result<(), SQLError> {
    let declared_types = declared.iter().map(ToString::to_string).collect::<Vec<_>>();
    let column_names = (1..=declared.len())
        .map(|position| format!("c{position}"))
        .collect::<Vec<_>>();
    validate_table_function_column_definition(
        &NoRoutines,
        ColumnDefinitionRequest {
            name,
            binding: None,
            user_function: None,
            user_invocation: None,
            args,
            declared_types: &declared_types,
            column_names: &column_names,
        },
        &RowSchema::default(),
        &[],
        &NoRoutines,
    )
}

#[test]
fn built_in_results_decide_column_definition_lists() {
    let integers = [ScalarExpr::Array(vec![ScalarExpr::Literal(
        uqa_core::Value::Int(1),
    )])];
    for (name, args) in [
        ("generate_series", &[][..]),
        ("regexp_matches", &[][..]),
        ("regexp_split_to_table", &[][..]),
        ("pg_catalog.unnest", &integers[..]),
        ("string_to_table", &[][..]),
        ("json_array_elements", &[][..]),
    ] {
        let error = column_definition(name, args, &["int4"]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42601"), "{name}");
        assert_eq!(
            error.to_string(),
            "a column definition list is only allowed for functions returning \"record\"",
            "{name}"
        );
        column_definition(name, args, &[]).unwrap();
    }
    for name in ["json_each", "jsonb_each_text", "aclexplode"] {
        assert_eq!(
            column_definition(name, &[], &["text"])
                .unwrap_err()
                .to_string(),
            "a column definition list is redundant for a function with OUT parameters"
        );
    }
    // UQA extensions and host-registered functions describe their own results.
    column_definition("cypher", &[], &["agtype"]).unwrap();
}

fn user_function(sql: &str) -> SQLUserFunction {
    let crate::Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0)
    else {
        unreachable!("CREATE FUNCTION fixture")
    };
    SQLUserFunction::new(*definition, RoutineBody::Source)
}

#[test]
fn user_functions_declare_their_from_columns_before_aliases() {
    let declared =
        |sql: &str| user_function_declared_columns(&NoRoutines, &user_function(sql), None).unwrap();
    // OUT parameters, including the columns of RETURNS TABLE, name the columns; an unnamed one takes its position.
    assert_eq!(
        declared(
            "CREATE FUNCTION f(OUT a int, OUT int) RETURNS SETOF record LANGUAGE sql AS 'SELECT 1, 2'"
        ),
        Some(vec!["a".to_string(), "column2".to_string()])
    );
    assert_eq!(
        declared("CREATE FUNCTION f(n int) RETURNS TABLE (u int) LANGUAGE sql AS 'SELECT n'"),
        Some(vec!["u".to_string()])
    );
    // The FROM item names the single column of any other result.
    assert_eq!(
        declared("CREATE FUNCTION f() RETURNS SETOF int LANGUAGE sql AS 'SELECT 1'"),
        None
    );
    assert_eq!(
        declared("CREATE FUNCTION f() RETURNS SETOF record LANGUAGE sql AS 'SELECT 1'"),
        None
    );
}

#[test]
fn scalar_from_aliases_do_not_create_columns() {
    for returns in ["int", "SETOF int", "TABLE (value int)"] {
        let function = user_function(&format!(
            "CREATE FUNCTION f() RETURNS {returns} LANGUAGE sql AS 'SELECT 1'"
        ));
        for ordinality in [false, true] {
            let aliases = if ordinality {
                vec!["a".into(), "b".into(), "c".into()]
            } else {
                vec!["a".into(), "b".into()]
            };
            let error = super::user_table_function_columns(
                &NoRoutines,
                &function,
                None,
                "f",
                Some("x"),
                &aliases,
                ordinality,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("42P10"));
            assert_eq!(
                error.to_string(),
                format!(
                    "table \"x\" has {} columns available but {} columns specified",
                    1 + usize::from(ordinality),
                    aliases.len()
                )
            );
        }
    }
    let function =
        user_function("CREATE FUNCTION f() RETURNS SETOF record LANGUAGE sql AS 'SELECT 1, 2'");
    assert_eq!(
        super::user_table_function_columns(
            &NoRoutines,
            &function,
            None,
            "f",
            Some("x"),
            &["a".into(), "b".into()],
            true
        )
        .unwrap(),
        ["a", "b", "ordinality"]
    );
}

#[test]
fn record_column_definitions_keep_postgresql_validation_order() {
    let function =
        user_function("CREATE FUNCTION r() RETURNS SETOF record LANGUAGE sql AS 'SELECT 1, 2'");
    for (names, types, state, message) in [
        (
            ["a", "b"],
            ["nosuchtype", "setof int"],
            "42704",
            "type \"nosuchtype\" does not exist",
        ),
        (
            ["a", "a"],
            ["int", "int"],
            "42701",
            "column name \"a\" specified more than once",
        ),
        (
            ["a", "b"],
            ["int", "anyelement"],
            "42P16",
            "column \"b\" has pseudo-type anyelement",
        ),
        (
            ["a", "a"],
            ["anyelement", "int"],
            "42701",
            "column name \"a\" specified more than once",
        ),
    ] {
        let error = validate_table_function_column_definition(
            &NoRoutines,
            ColumnDefinitionRequest {
                name: "r",
                binding: None,
                user_function: Some(&function),
                user_invocation: None,
                args: &[],
                declared_types: &types.map(String::from),
                column_names: &names.map(String::from),
            },
            &RowSchema::default(),
            &[],
            &NoRoutines,
        )
        .expect_err(message);
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn controlled_empty_function_columns_admit_only_final_names() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let unused = "x".repeat(4 << 20);
    let aliases = vec!["selected".to_owned(), "position".to_owned()];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            super::table_function_empty_schema_with_control(
                "generate_series",
                &unused,
                Some(&unused),
                &aliases,
                1,
                true,
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(*output, aliases);
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(measured.bytes_current, output.reserved_bytes() as i64);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            super::table_function_empty_schema_with_control(
                "generate_series",
                &unused,
                None,
                &[],
                1,
                false,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert_eq!(
        super::table_function_empty_schema_with_control(
            "generate_series",
            "series",
            None,
            &[],
            1,
            false,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}
