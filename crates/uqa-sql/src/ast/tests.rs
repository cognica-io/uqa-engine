//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AlterSequence, ColumnType, CreateFunction, CreateTrigger, FunctionBinding, FunctionBody,
    FunctionDispatch, FunctionParallel, FunctionParam, FunctionParamMode, FunctionReturns,
    FunctionVolatility, RangeFunctionOperation, RangeSubtype, RoutineAclEntry,
    RoutineInvocationBinding, RoutineSecurityAttributes, RoutineVariadicMode, SequenceRestart,
    Statement, TriggerDeferrability,
};

#[test]
fn reg_alias_scalar_and_array_names_preserve_type_identity() {
    assert_eq!(
        ColumnType::from_sql_name("pg_catalog.regclass").unwrap(),
        ColumnType::Regclass
    );
    assert_eq!(
        ColumnType::from_sql_name("_regclass").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Regclass))
    );
    assert_eq!(ColumnType::Regclass.sql_name(), "regclass");
    assert_eq!(
        ColumnType::from_sql_name("pg_catalog.regprocedure").unwrap(),
        ColumnType::Regprocedure
    );
    assert_eq!(
        ColumnType::from_sql_name("_regprocedure").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Regprocedure))
    );
    assert_eq!(ColumnType::Regprocedure.sql_name(), "regprocedure");
    assert_eq!(
        ColumnType::from_sql_name("pg_catalog.regrole").unwrap(),
        ColumnType::Regrole
    );
    assert_eq!(
        ColumnType::from_sql_name("_regrole").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Regrole))
    );
    assert_eq!(ColumnType::Regrole.sql_name(), "regrole");
}

#[test]
fn legacy_routine_acl_entries_default_the_grantor_to_the_owner() {
    let entry: RoutineAclEntry = serde_json::from_value(serde_json::json!({
        "role": "routine_caller",
        "grant_option": true
    }))
    .unwrap();
    assert_eq!(entry.role, "routine_caller");
    assert_eq!(entry.grantor, None);
    assert!(entry.grant_option);
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
        dispatch: None,
        invocation: None,
        resolution_error: None,
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
        dispatch: None,
        invocation: None,
        resolution_error: None,
    };
    assert!(!fixed.is_polymorphic_builtin_syntax());
}

#[test]
fn legacy_compiler_function_names_upgrade_only_at_the_catalog_boundary() {
    let mut parser_name = "__subscript".to_string();
    let mut parser_binding = None;
    assert!(FunctionBinding::upgrade_legacy_serialized_dispatch(
        &mut parser_name,
        &mut parser_binding
    ));
    assert_eq!(
        parser_binding.and_then(|binding| binding.dispatch),
        Some(FunctionDispatch::Subscript)
    );
    assert_eq!(parser_name, "subscript");

    let mut builtin_name = "__to_hex_int4".to_string();
    let mut builtin_binding = Some(FunctionBinding {
        name: "pg_catalog.to_hex".into(),
        argument_types: vec!["integer".into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    });
    assert!(FunctionBinding::upgrade_legacy_serialized_dispatch(
        &mut builtin_name,
        &mut builtin_binding
    ));
    assert_eq!(builtin_name, "pg_catalog.to_hex");
    assert_eq!(
        builtin_binding.and_then(|binding| binding.dispatch),
        Some(FunctionDispatch::ToHexInt4)
    );

    let mut user_name = "__subscript".to_string();
    let user_binding = FunctionBinding {
        name: "app.__subscript".into(),
        argument_types: vec!["integer".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    };
    let mut user_binding_slot = Some(user_binding.clone());
    assert!(!FunctionBinding::upgrade_legacy_serialized_dispatch(
        &mut user_name,
        &mut user_binding_slot
    ));
    assert_eq!(user_name, "__subscript");
    assert_eq!(user_binding_slot, Some(user_binding));

    assert_eq!(
        FunctionDispatch::from_legacy_serialized_name("__range_contained_by_tstzmultirange"),
        Some(FunctionDispatch::Range {
            operation: RangeFunctionOperation::ContainedBy,
            subtype: RangeSubtype::TimestampTz,
            multirange: true,
        })
    );
    assert_eq!(
        FunctionDispatch::from_legacy_serialized_name("app.__subscript"),
        None
    );
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
        dispatch: None,
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![0, 1],
            argument_targets: vec!["integer".into(), "integer[]".into()],
            parameter_types: vec!["integer".into(), "integer[]".into()],
            return_type: Some("integer".into()),
            variadic_mode: RoutineVariadicMode::Explicit { parameter_index: 1 },
        })),
        resolution_error: None,
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

#[test]
fn trigger_deferrability_defaults_for_legacy_catalogs_and_round_trips() {
    let Statement::CreateTrigger(ordinary) = crate::compile(
        "CREATE TRIGGER ordinary AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION probe()",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected CREATE TRIGGER");
    };
    let mut legacy = serde_json::to_value(&ordinary).unwrap();
    {
        let object = legacy.as_object_mut().unwrap();
        object.remove("constraint");
        object.remove("referenced_table");
        object.remove("deferrability");
    }
    let restored: CreateTrigger = serde_json::from_value(legacy).unwrap();
    assert!(!restored.constraint);
    assert_eq!(restored.referenced_table, None);
    assert_eq!(restored.deferrability, TriggerDeferrability::NotDeferrable);

    let Statement::CreateTrigger(deferred) = crate::compile(
        "CREATE CONSTRAINT TRIGGER deferred AFTER INSERT ON items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION probe()",
    )
    .unwrap()
    .remove(0)
    else {
        panic!("expected CREATE CONSTRAINT TRIGGER");
    };
    assert_eq!(
        serde_json::from_value::<CreateTrigger>(serde_json::to_value(&deferred).unwrap())
            .unwrap()
            .deferrability,
        TriggerDeferrability::InitiallyDeferred
    );
}
