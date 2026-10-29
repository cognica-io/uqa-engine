//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_syntax_copy_preserves_transient_routine_source_positions() {
    let source = one("CREATE FUNCTION f() RETURNS text LANGUAGE SQL AS 'SELECT ''문자열''' ");
    let UnifiedPlan::Command(command) = &source else {
        panic!("command");
    };
    let CommandPlan::CreateFunction(function) = command.as_ref() else {
        panic!("function");
    };
    assert!(function.body_origin.is_some());
    assert_copy(&source);
}

#[test]
fn complete_syntax_copy_retains_stored_merge_target_identities_and_domain_dependencies() {
    let mut statement =
        crate::compile("MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN UPDATE SET a = u.a")
            .unwrap()
            .remove(0);
    let crate::Statement::Merge(merge) = &mut statement else {
        panic!("merge");
    };
    merge.target_column_bindings.insert(
        "a".into(),
        crate::ast::MergeTargetColumnBinding {
            object_id: [17; 16],
            attribute_number: Some(3),
            domain_dependencies: [16001, 16002].into_iter().collect(),
        },
    );
    let mut source = one("CREATE PROCEDURE p() LANGUAGE SQL BEGIN ATOMIC SELECT 1; END");
    let UnifiedPlan::Command(command) = &mut source else {
        panic!("command");
    };
    let CommandPlan::CreateFunction(function) = command.as_mut() else {
        panic!("procedure");
    };
    function.body = crate::ast::FunctionBody::Statements(vec![statement]);
    assert_copy(&source);
    let wire = serde_json::to_value(&source).unwrap();
    let decoded: UnifiedPlan = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}

fn raw_window(source: &UnifiedPlan) -> &crate::ast::WindowDefinitionSyntax {
    let UnifiedPlan::Command(command) = source else {
        panic!("command");
    };
    let CommandPlan::CreateFunction(function) = command.as_ref() else {
        panic!("function");
    };
    let crate::ast::FunctionBody::Statements(statements) = &function.body else {
        panic!("statements");
    };
    let crate::Statement::Select(select) = &statements[0] else {
        panic!("select");
    };
    select.windows[0].spec.raw_definition.as_ref().unwrap()
}

#[test]
fn complete_syntax_copy_shares_immutable_window_identity_without_a_json_copy() {
    let mut source = one("CREATE FUNCTION f() RETURNS bigint LANGUAGE SQL BEGIN ATOMIC SELECT sum(a) OVER w FROM t WINDOW w AS (ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW); END");
    let UnifiedPlan::Command(command) = &mut source else {
        panic!("command");
    };
    let CommandPlan::CreateFunction(function) = command.as_mut() else {
        panic!("function");
    };
    let crate::ast::FunctionBody::Statements(statements) = &mut function.body else {
        panic!("statements");
    };
    let crate::Statement::Select(select) = &mut statements[0] else {
        panic!("select");
    };
    select.windows[0].spec.raw_definition = Some(crate::ast::WindowDefinitionSyntax(Arc::new(
        serde_json::json!({"order": "x".repeat(4 * 1024 * 1024)}),
    )));
    let original = raw_window(&source);
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let copied = source
        .clone_with_control(&ProductionControl::new(&budget, &token, &token))
        .unwrap();
    assert!(Arc::ptr_eq(&original.0, &raw_window(&copied).0));
    assert_copy(&source);
    drop(copied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_syntax_copy_preserves_unbound_parameter_carriers_without_observing_errors() {
    let tensor = crate::SQLParam::tensor_with_control(
        &[1.0, -0.0, 2.0, 3.0],
        2,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    let params = vec![
        crate::SQLParam::Scalar(Value::Str("untyped".into())),
        crate::SQLParam::TypedScalar {
            value: Value::Null,
            ty: crate::ColumnType::Array(Box::new(crate::ColumnType::Named("named".into()))),
        },
        crate::SQLParam::DeferredError {
            ty: crate::ColumnType::Text,
            error: Box::new(crate::ast::DeferredSQLError {
                sqlstate: "42703".into(),
                message: "field does not exist".into(),
                detail: Some("original detail".into()),
                hint: Some("original hint".into()),
            }),
        },
        crate::SQLParam::Vector(vec![-0.0, f32::from_bits(0x7fc0_0042)]),
        crate::SQLParam::Tensor(vec![vec![1.0], vec![2.0, 3.0]]),
        tensor,
    ];
    let source = UnifiedPlan::Command(Box::new(CommandPlan::Explain {
        options: crate::explain::ExplainOptions::default(),
        body: Box::new(one("SELECT $1")),
        unbound_parameters: Some(params),
    }));
    assert_copy(&source);
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let copied = source
        .clone_with_control(&ProductionControl::new(&budget, &token, &token))
        .unwrap();
    let UnifiedPlan::Command(command) = &*copied else {
        panic!("command");
    };
    let CommandPlan::Explain {
        unbound_parameters: Some(params),
        ..
    } = command.as_ref()
    else {
        panic!("explain parameters");
    };
    let crate::SQLParam::Vector(vector) = &params[3] else {
        panic!("vector");
    };
    assert_eq!(vector[0].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(vector[1].to_bits(), 0x7fc0_0042);
    assert_eq!(params[2].to_value().unwrap_err().sqlstate(), Some("42703"));
    assert_eq!(
        params[4].to_value().unwrap(),
        Value::List(vec![
            Value::List(vec![Value::Float(1.0)]),
            Value::List(vec![Value::Float(2.0), Value::Float(3.0)])
        ])
    );
    drop(copied);
    assert_eq!(budget.used(), 0);
}
