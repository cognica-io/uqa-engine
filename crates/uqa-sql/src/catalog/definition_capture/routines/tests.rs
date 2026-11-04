//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, FunctionBody, Statement},
    plan::{CommandPlan, RelationalPlan},
    ScalarExpr,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn durable_routines_borrow_quoted_names_and_exclude_temporary_bodies_before_admission() {
    let mut source = BTreeMap::from([
        (
            "\"public\".\"pg_temp_7.f\"".into(),
            vec![Arc::new(routine(
                "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 1",
            ))],
        ),
        ("\"pg_temp\".\"f\"".into(), Vec::new()),
    ]);
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let baseline =
        allocation_counter::measure(|| saved = Some(routines(&source, &control).unwrap()));
    let bytes = memory.used();
    drop(saved.take());
    for name in [
        "pg_temp_7.f",
        "\"pg_toast_temp_7\".\"f\"",
        "\"pg_temp_7.dot\".\"f\"\"quote\"",
    ] {
        let mut temporary =
            routine("CREATE FUNCTION f() RETURNS text LANGUAGE SQL AS 'unexamined'");
        temporary.def.body = FunctionBody::Source("x".repeat(1 << 20));
        source.insert(name.into(), vec![Arc::new(temporary)]);
    }
    let selected =
        allocation_counter::measure(|| saved = Some(routines(&source, &control).unwrap()));
    assert_eq!(selected.bytes_total, baseline.bytes_total);
    assert_eq!(selected.bytes_current, baseline.bytes_current);
    assert_eq!(memory.used(), bytes);
    let saved = saved.unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].0, "\"pg_temp\".\"f\"");
    assert!(saved[0].1.is_empty());
    assert_eq!(saved[1].0, "\"public\".\"pg_temp_7.f\"");
    assert!(saved[1].1[0].bound_body.is_some());
    drop(saved);
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        routines(&source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_routines_reject_invalid_catalog_names_with_the_original_allowance() {
    let source = BTreeMap::from([("\"unterminated".into(), Vec::new())]);
    for limit in [0, 4096] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let error =
            routines(&source, &ProductionControl::new(&memory, &token, &token)).unwrap_err();
        if limit == 0 {
            assert_eq!(error.sqlstate(), Some("53200"));
        } else {
            assert!(
                matches!(error, SQLError::Internal(message) if message.contains("invalid stored routine name"))
            );
        }
        assert_eq!(memory.used(), 0);
    }
}

fn routine(sql: &str) -> SQLUserFunction {
    let Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("routine")
    };
    let body = match &definition.body {
        FunctionBody::Source(_) => RoutineBody::Source,
        FunctionBody::Statements(statements) => RoutineBody::Bound(Arc::new(
            CompiledFunctionBody::SQL(statements.iter().cloned().map(UnifiedPlan::lower).collect()),
        )),
    };
    SQLUserFunction::new(*definition, body)
}

#[test]
fn routine_capture_preserves_overload_order_attributes_identities_and_bound_bodies() {
    let mut source = BTreeMap::from([("empty".into(), Vec::new())]);
    let entries = source.entry("Quoted.Routine".into()).or_default();
    for (index, sql) in [
        "CREATE FUNCTION f(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL IMMUTABLE STRICT SECURITY DEFINER PARALLEL SAFE COST 42 SET search_path TO public RETURN a + 1",
        "CREATE FUNCTION f(a text) RETURNS text LANGUAGE SQL AS 'SELECT a'",
        "CREATE FUNCTION f() RETURNS integer LANGUAGE plpgsql AS 'not compiled during capture'",
        "CREATE PROCEDURE p() LANGUAGE SQL BEGIN ATOMIC INSERT INTO docs VALUES (1); UPDATE docs SET id = 2; DELETE FROM docs WHERE id = 3; END",
        "CREATE FUNCTION w() RETURNS bigint LANGUAGE SQL BEGIN ATOMIC SELECT sum(id) OVER w FROM docs WINDOW w AS (ORDER BY id); END",
    ].into_iter().enumerate() {
        let mut value = routine(sql);
        value.def.object_id = Some([u8::try_from(index).unwrap();16]);
        value.def.catalog_revision = Some([19;16]);
        value.def.catalog_oid = Some(30000 + u32::try_from(index).unwrap());
        value.def.creation_search_path = vec!["Quoted.Schema".into(), "public".into()];
        value.def.execute_acl = Some(Vec::new());
        value.definition_version().unwrap();
        entries.push(Arc::new(value));
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut captured = None;
    let allocation =
        allocation_counter::measure(|| captured = Some(routines(&source, &control).unwrap()));
    let captured = captured.unwrap();
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    for ((name, entries), (old_name, originals)) in captured.iter().zip(&source) {
        assert_eq!(name, old_name);
        assert_eq!(entries.len(), originals.len());
        for (value, original) in entries.iter().zip(originals) {
            assert_eq!(
                serde_json::to_value(&value.definition).unwrap(),
                serde_json::to_value(&original.def).unwrap()
            );
            assert!(value.definition.body_origin.is_none());
            let reconstructed = SQLUserFunction::new(value.definition.clone(), RoutineBody::Source);
            assert_eq!(
                reconstructed.definition_version().unwrap(),
                original.definition_version().unwrap()
            );
            match (&value.bound_body, &original.body) {
                (None, RoutineBody::Source) => {}
                (Some(plans), RoutineBody::Bound(body)) => {
                    let CompiledFunctionBody::SQL(old) = body.as_ref() else {
                        panic!("SQL")
                    };
                    assert_eq!(
                        serde_json::to_value(plans).unwrap(),
                        serde_json::to_value(old).unwrap()
                    );
                }
                _ => panic!("body form changed"),
            }
        }
    }
    drop((captured, source));
    assert_eq!(memory.used(), 0);
}

fn retained_literal(control: &ProductionControl<'_>) -> crate::ir::RetainedScalarLiteral {
    let value = crate::ir::RetainedConstantValue::from_produced(
        control.copy_value(&Value::Str("x".repeat(65536))).unwrap(),
        control,
    )
    .unwrap();
    crate::ir::RetainedScalarLiteral::with_control(value, None, None, Some(950), control).unwrap()
}

#[test]
fn routine_capture_detaches_constants_and_discards_transient_source_window_and_parameter_owners() {
    for uncontrolled in [false, true] {
        let mut original=routine("CREATE FUNCTION f(a text DEFAULT 'original') RETURNS text LANGUAGE SQL BEGIN ATOMIC SELECT sum(id) OVER w FROM docs WINDOW w AS (ORDER BY id); END");
        let source_memory = MemoryBudget::new(1 << 20);
        let source_token = CancellationToken::new();
        let source_control = ProductionControl::new(&source_memory, &source_token, &source_token);
        let literal = retained_literal(&source_control);
        original.def.params[0].default = Some(Expr::RetainedLiteral(Box::new(
            crate::ast::RetainedInputLiteral(literal.clone()),
        )));
        let FunctionBody::Statements(statements) = &mut original.def.body else {
            panic!("statements")
        };
        let Statement::Select(query) = &mut statements[0] else {
            panic!("query")
        };
        let raw = Arc::new(serde_json::json!({"raw":"y".repeat(1<<20)}));
        let weak = Arc::downgrade(&raw);
        query.windows[0].spec.raw_definition = Some(crate::ast::WindowDefinitionSyntax(raw));
        let UnifiedPlan::Query(mut query) =
            UnifiedPlan::lower(crate::compile("SELECT 'payload'").unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.projections[0].expr = ScalarExpr::RetainedLiteral(literal);
        original.body = RoutineBody::Bound(Arc::new(CompiledFunctionBody::SQL(vec![
            UnifiedPlan::Command(Box::new(CommandPlan::Explain {
                options: crate::explain::ExplainOptions::default(),
                body: Box::new(UnifiedPlan::Query(query)),
                unbound_parameters: Some(vec![crate::SQLParam::Scalar(Value::Str(
                    "z".repeat(1 << 20),
                ))]),
            })),
        ])));
        source_token.cancel();
        let memory = MemoryBudget::new(256 * 1024);
        let token = CancellationToken::new();
        let control = if uncontrolled {
            ProductionControl::uncontrolled()
        } else {
            ProductionControl::new(&memory, &token, &token)
        };
        let saved = original.capture_definition(&control).unwrap();
        let FunctionBody::Statements(statements) = &saved.definition.body else {
            panic!("statements")
        };
        let Statement::Select(query) = &statements[0] else {
            panic!("query")
        };
        assert!(query.windows[0].spec.raw_definition.is_none());
        let Some(Expr::RetainedLiteral(literal)) = &saved.definition.params[0].default else {
            panic!("default")
        };
        assert_eq!(literal.0.collation_oid, Some(950));
        let UnifiedPlan::Command(command) = &saved.bound_body.as_ref().unwrap()[0] else {
            panic!("command")
        };
        let CommandPlan::Explain {
            unbound_parameters,
            body,
            ..
        } = command.as_ref()
        else {
            panic!("explain")
        };
        assert!(unbound_parameters.is_none());
        let UnifiedPlan::Query(query) = body.as_ref() else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        let ScalarExpr::RetainedLiteral(bound) = &block.projections[0].expr else {
            panic!("bound constant")
        };
        assert!(!bound.value.shares_value_with(&literal.0.value));
        drop(original);
        assert!(weak.upgrade().is_none());
        assert_eq!(source_memory.used(), 0);
        assert!(bound
            .value
            .resident_value()
            .unwrap()
            .has_same_representation(&Value::Str("x".repeat(65536))));
        drop(saved);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn routine_capture_releases_earlier_overloads_on_late_denial_or_cancellation() {
    let mut large = routine("CREATE FUNCTION large() RETURNS text LANGUAGE SQL AS 'unexamined'");
    large.def.body = FunctionBody::Source("x".repeat(65536));
    let source = BTreeMap::from([(
        "routines".into(),
        vec![
            Arc::new(routine(
                "CREATE FUNCTION first() RETURNS integer LANGUAGE SQL RETURN 1",
            )),
            Arc::new(large),
        ],
    )]);
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 1 << 20 } else { 16384 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let error =
            routines(&source, &ProductionControl::new(&memory, &token, &token)).unwrap_err();
        if cancelled {
            assert!(matches!(error, SQLError::Cancelled(_)));
        } else {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn routine_capture_rejects_execution_only_rows_in_stored_syntax() {
    let mut source = routine(
        "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1 FROM docs; END",
    );
    let FunctionBody::Statements(statements) = &mut source.def.body else {
        panic!("statements")
    };
    let Statement::Select(query) = &mut statements[0] else {
        panic!("query")
    };
    let relation = crate::ast::InternalRelationId::allocate();
    let schema = crate::RowSchema::with_internal_relation_types(
        relation,
        vec![Some(crate::ColumnType::Integer)],
    );
    query.from = Some(crate::ast::FromClause::RetainedRows(
        crate::ast::RetainedRowSource::new(relation, schema, 1).unwrap(),
    ));
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let error = source
        .capture_definition(&ProductionControl::new(&memory, &token, &token))
        .unwrap_err();
    assert!(
        matches!(error,SQLError::Internal(message) if message.contains("execution-scoped row source"))
    );
    assert_eq!(memory.used(), 0);
}
