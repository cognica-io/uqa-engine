//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AlterSequence, ColumnType, CreateFunction, FunctionBinding, FunctionBody, FunctionParallel,
    FunctionParam, FunctionParamMode, FunctionReturns, FunctionVolatility,
    RoutineInvocationBinding, RoutineSecurityAttributes, RoutineVariadicMode, SequenceRestart,
    Statement,
};

#[test]
fn regclass_scalar_and_array_names_preserve_type_identity() {
    assert_eq!(
        ColumnType::from_sql_name("pg_catalog.regclass").unwrap(),
        ColumnType::Regclass
    );
    assert_eq!(
        ColumnType::from_sql_name("_regclass").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Regclass))
    );
    assert_eq!(ColumnType::Regclass.sql_name(), "regclass");
}

#[test]
fn regtype_output_omits_type_modifiers() {
    assert_eq!(
        ColumnType::Varchar(Some(7)).regtype_name(),
        "character varying"
    );
    assert_eq!(
        ColumnType::Numeric {
            precision: Some(10),
            scale: Some(2),
        }
        .regtype_name(),
        "numeric"
    );
    assert_eq!(ColumnType::Vector(3).regtype_name(), "vector");
    assert_eq!(
        ColumnType::Array(Box::new(ColumnType::Character(4))).regtype_name(),
        "character[]"
    );
}

#[test]
fn alter_sequence_restart_reads_legacy_and_current_serde_shapes() {
    let omitted: AlterSequence = serde_json::from_str(r#"{"name":"s"}"#).unwrap();
    assert_eq!(omitted.restart, SequenceRestart::Unchanged);

    let legacy_none: AlterSequence =
        serde_json::from_str(r#"{"name":"s","restart":null}"#).unwrap();
    assert_eq!(legacy_none.restart, SequenceRestart::Unchanged);

    let legacy_value: AlterSequence = serde_json::from_str(r#"{"name":"s","restart":7}"#).unwrap();
    assert_eq!(legacy_value.restart, SequenceRestart::With(7));

    let current = AlterSequence {
        name: "s".into(),
        restart: SequenceRestart::FromStart,
        ..AlterSequence::default()
    };
    let round_trip: AlterSequence =
        serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
    assert_eq!(round_trip.restart, SequenceRestart::FromStart);
}

#[test]
fn function_binding_builtin_identity_is_backward_compatible() {
    let legacy: FunctionBinding =
        serde_json::from_str(r#"{"name":"app.f","argument_types":["text"]}"#).unwrap();
    assert!(!legacy.builtin);

    let builtin = FunctionBinding {
        name: "pg_catalog.reverse".into(),
        argument_types: vec!["text".into()],
        builtin: true,
        invocation: None,
    };
    let encoded = serde_json::to_string(&builtin).unwrap();
    assert!(encoded.contains(r#""builtin":true"#));
    assert_eq!(
        serde_json::from_str::<FunctionBinding>(&encoded).unwrap(),
        builtin
    );
}

#[test]
fn polymorphic_builtin_syntax_binding_has_stable_serde_shape() {
    for name in ["coalesce", "greatest", "least", "nullif"] {
        let binding = FunctionBinding::polymorphic_builtin_syntax(name);
        assert!(binding.is_polymorphic_builtin_syntax());
        assert_eq!(
            serde_json::to_value(&binding).unwrap(),
            serde_json::json!({
                "name": name,
                "argument_types": [],
                "builtin": true
            })
        );
    }
    for ordinary_name in ["upper", "\"coalesce\"", "ordinary.coalesce"] {
        assert!(!FunctionBinding::is_polymorphic_builtin_syntax_name(
            ordinary_name
        ));
    }
    let fixed = FunctionBinding {
        name: "coalesce".into(),
        argument_types: vec!["text".into(), "text".into()],
        builtin: true,
        invocation: None,
    };
    assert!(!fixed.is_polymorphic_builtin_syntax());
}

#[test]
fn routine_invocation_binding_round_trips_and_legacy_bindings_default_to_none() {
    let legacy: FunctionBinding =
        serde_json::from_str(r#"{"name":"app.f","argument_types":["anyelement"]}"#).unwrap();
    assert!(legacy.invocation.is_none());

    let binding = FunctionBinding {
        name: "app.f".into(),
        argument_types: vec!["anyelement".into(), "anyarray".into()],
        builtin: false,
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![0, 1],
            argument_targets: vec!["integer".into(), "integer[]".into()],
            parameter_types: vec!["integer".into(), "integer[]".into()],
            return_type: Some("integer".into()),
            variadic_mode: RoutineVariadicMode::Explicit { parameter_index: 1 },
        })),
    };
    let encoded = serde_json::to_value(&binding).unwrap();
    assert_eq!(
        encoded["invocation"]["variadic_mode"],
        serde_json::json!({ "Explicit": { "parameter_index": 1 } })
    );
    assert_eq!(
        serde_json::from_value::<FunctionBinding>(encoded).unwrap(),
        binding
    );
}

#[test]
fn routine_identity_and_call_parameters_are_distinct() {
    let param = |name: &str, mode| FunctionParam {
        name: name.into(),
        type_name: "integer".into(),
        type_reference: None,
        mode,
        default: None,
    };
    let function = CreateFunction {
        name: "f".into(),
        or_replace: false,
        is_procedure: false,
        params: vec![
            param("input", FunctionParamMode::In),
            param("output", FunctionParamMode::Out),
            param("inout", FunctionParamMode::InOut),
            param("rest", FunctionParamMode::Variadic),
            param("table_column", FunctionParamMode::Table),
        ],
        returns: FunctionReturns::None,
        return_type_reference: None,
        language: "sql".into(),
        body: FunctionBody::Statements(Vec::new()),
        creation_search_path: Vec::new(),
        volatility: FunctionVolatility::Volatile,
        strict: false,
        owner: String::new(),
        security: RoutineSecurityAttributes::default(),
        parallel: FunctionParallel::Unsafe,
        support: None,
        config: Vec::new(),
        config_actions: Vec::new(),
        execute_acl: None,
    };

    let identity_names = function
        .identity_params()
        .into_iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(identity_names, ["input", "inout", "rest"]);
    assert_eq!(function.identity_arity(), 3);
    let function_call_names = function
        .call_params()
        .into_iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(function_call_names, identity_names);
    assert_eq!(function.call_arity(), 3);
    assert_eq!(function.required_call_arity(), 2);
    assert_eq!(function.signature_arity(), function.call_arity());

    let mut procedure = function.clone();
    procedure.is_procedure = true;
    let procedure_call_names = procedure
        .call_params()
        .into_iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(procedure_call_names, ["input", "output", "inout", "rest"]);
    assert_eq!(procedure.identity_arity(), 3);
    assert_eq!(procedure.call_arity(), 4);
    assert_eq!(procedure.required_call_arity(), 3);

    let mut legacy = serde_json::to_value(&function).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("creation_search_path");
    assert!(serde_json::from_value::<CreateFunction>(legacy)
        .unwrap()
        .creation_search_path
        .is_empty());
    let mut persisted = function;
    persisted.creation_search_path = vec!["app".into(), "public".into()];
    assert_eq!(
        serde_json::from_value::<CreateFunction>(serde_json::to_value(&persisted).unwrap())
            .unwrap()
            .creation_search_path,
        ["app", "public"]
    );
}

#[test]
fn create_table_as_reads_legacy_statements_without_optional_fields() {
    let mut statement = crate::compile("CREATE TABLE copy AS SELECT 1")
        .unwrap()
        .remove(0);
    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = &statement
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(column_names.is_empty());
    assert!(!with_no_data);

    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = &mut statement
    else {
        unreachable!();
    };
    column_names.push("renamed".into());
    *with_no_data = true;
    let mut encoded = serde_json::to_value(statement).unwrap();
    let fields = encoded["CreateTableAs"].as_object_mut().unwrap();
    fields.remove("column_names");
    fields.remove("with_no_data");
    let legacy: Statement = serde_json::from_value(encoded).unwrap();
    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = legacy
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(column_names.is_empty());
    assert!(!with_no_data);
}
