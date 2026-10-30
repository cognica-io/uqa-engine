//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;
use uqa_planner::statement_planning::schema_expressions::optimize_catalog_scalar;
use uqa_sql::{
    ast::{
        CompositeFieldBinding, CompositeRowBinding, FunctionBinding, FunctionDispatch,
        FunctionOrderSyntax,
    },
    ColumnType, ScalarExpr,
};

const BYTES: usize = 4 << 20;

fn input(ty: &ColumnType, field: bool) -> ScalarExpr {
    if !field {
        return ScalarExpr::CompositeRow {
            items: vec![
                ScalarExpr::Literal(Value::Int(7)),
                ScalarExpr::Literal(Value::Str("x".repeat(BYTES))),
            ],
            binding: CompositeRowBinding {
                layout: None,
                ty: ty.catalog_name(),
                attributes: vec![1, 2],
                argument_types: Some(vec![ColumnType::Integer, ColumnType::Text]),
            },
            bound_type: Some(ty.clone()),
        };
    }
    let ColumnType::Composite(reference) = ty else {
        panic!("composite")
    };
    let mut binding = FunctionBinding::builtin_by_name("field selection", None);
    binding.dispatch = Some(FunctionDispatch::FieldSelect);
    binding.set_composite_field(CompositeFieldBinding {
        type_oid: reference.oid,
        number: 2,
        result_type: ColumnType::Text,
        collation_oid: None,
        dropped: false,
        changed_type: None,
    });
    ScalarExpr::Func {
        name: "field selection".into(),
        binding: Some(binding),
        args: vec![
            ScalarExpr::TypedLiteral {
                value: Value::Record(uqa_core::RecordValue::from_parts(
                    vec![
                        ("a".into(), Value::Int(7)),
                        ("b".into(), Value::Str("x".repeat(BYTES))),
                    ],
                    Some(reference.oid),
                )),
                ty: ty.catalog_name(),
                bound_type: Some(ty.clone()),
                parameter_index: None,
                collation_oid: None,
                composite_source: None,
            },
            ScalarExpr::Literal(Value::Str("b".into())),
        ],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::Ordinary,
        filter: None,
    }
}

fn verify(provider: Provider, field: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("composite-constant.db"));
    engine
        .sql("CREATE TYPE planning_pair AS (a integer, b text)", &[])
        .unwrap();
    let context = engine.statement_planning_context();
    let types = context.optimization.routine_inlining().unwrap().types;
    let ty = types
        .resolve_catalog_column_type_name("planning_pair")
        .unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let baseline = memory.used();
    for available in [0, 8192] {
        let occupied = memory.reserve(memory.available() - available).unwrap();
        let mut expression = input(&ty, field);
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(optimize_catalog_scalar(&context, &mut expression));
        });
        assert_eq!(
            result.unwrap().unwrap_err().sqlstate(),
            Some("53200"),
            "{provider:?}, field={field}, {allocated:?}"
        );
        assert!(
            allocated.bytes_total < 32 * 1024,
            "{provider:?}, field={field}: {allocated:?}"
        );
        drop(expression);
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
    }
    let mut expression = input(&ty, field);
    engine.cancel();
    assert_eq!(
        optimize_catalog_scalar(&context, &mut expression)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(expression);
    assert_eq!(memory.used(), baseline);
    engine.reset_cancellation();

    let mut expression = input(&ty, field);
    optimize_catalog_scalar(&context, &mut expression).unwrap();
    assert!(memory.used() >= baseline + BYTES);
    let retained = memory.used();
    let mut copies = None;
    let allocations = allocation_counter::measure(|| {
        copies = Some(std::array::from_fn::<_, 16, _>(|_| expression.clone()));
    });
    assert_eq!(allocations.count_total, 0);
    drop(expression);
    assert_eq!(memory.used(), retained);
    let copies = copies.unwrap();
    let ScalarExpr::RetainedLiteral(literal) = &copies[0] else {
        panic!("retained constant")
    };
    let value = literal.value.resident_value().unwrap();
    let output = if field {
        value.clone()
    } else {
        let Value::Datum(datum) = value else {
            panic!("retained tuple")
        };
        let selected = uqa_sql::expr::datums::read_composite_field_with_control(
            datum,
            "b",
            types.composite_types(),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        let Value::Datum(datum) = &*selected else {
            panic!("retained field")
        };
        uqa_sql::expr::datums::read(datum).unwrap()
    };
    assert_eq!(output, Value::Str("x".repeat(BYTES)));
    drop(copies);
    assert_eq!(memory.used(), baseline);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_composite_constructor_admits_constant_output_and_recovers(#[case] provider: Provider) {
    verify(provider, false);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_composite_field_copy_admits_constant_output_and_recovers(#[case] provider: Provider) {
    verify(provider, true);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_prepared_collated_fields_keep_owned_identity_across_rename(#[case] provider: Provider) {
    for mode in ["force_generic_plan", "force_custom_plan"] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("prepared-field.db"));
        engine.sql("CREATE TYPE saved_pair AS(a integer, b text); CREATE TABLE saved_rows(v saved_pair CHECK ((v).b <> 'bad')); INSERT INTO saved_rows VALUES('(1,x)'); PREPARE saved(text) AS SELECT (v).b AS picked FROM saved_rows WHERE (v).b=$1", &[]).unwrap();
        engine
            .sql(&format!("SET plan_cache_mode={mode}"), &[])
            .unwrap();
        let read = || {
            let result = engine.sql("EXECUTE saved('x')", &[]).unwrap();
            assert_eq!(result.columns, ["picked"]);
            let picked = match &result.rows[0]["picked"] {
                Value::Datum(datum) => uqa_sql::expr::datums::read(datum).unwrap(),
                value => value.clone(),
            };
            assert_eq!(picked, Value::Str("x".into()));
        };
        read();
        let retained = engine.session.prepared.read()["saved"].logical_plan.clone();
        let retention = engine.query_retention_control().unwrap();
        let memory = retention.memory();
        let peer = match provider {
            Provider::Memory => None,
            _ => Some(engine.new_session().unwrap()),
        };
        peer.as_ref()
            .unwrap_or(&engine)
            .sql("ALTER TYPE saved_pair RENAME ATTRIBUTE b TO label", &[])
            .unwrap();
        read();
        assert_eq!(
            engine
                .sql("SELECT (v).b FROM saved_rows", &[])
                .unwrap_err()
                .sqlstate(),
            Some("42703")
        );
        engine
            .sql(
                "BEGIN; ALTER TYPE saved_pair RENAME ATTRIBUTE label TO transient",
                &[],
            )
            .unwrap();
        read();
        engine.sql("ROLLBACK", &[]).unwrap();
        read();
        let entry = engine.session.prepared.read()["saved"].clone();
        if mode == "force_generic_plan" {
            assert_eq!(entry.generic_plans, 4);
            assert_eq!(entry.custom_plans, 0);
        } else {
            assert_eq!(entry.generic_plans, 0);
            assert_eq!(entry.custom_plans, 4);
        }
        drop(entry);
        let mut selected = 0;
        retained.visit_scalar_expressions(&mut |root| {
            root.visit(&mut |node| {
                if let ScalarExpr::Func {
                    binding: Some(binding),
                    args,
                    ..
                } = node
                {
                    if binding.dispatch == Some(FunctionDispatch::FieldSelect) {
                        assert_eq!(binding.composite_field().unwrap().number, 2);
                        assert!(
                            matches!(&args[1], ScalarExpr::Literal(Value::Str(name)) if name == "b")
                        );
                        selected += 1;
                    }
                }
            });
        });
        assert_eq!(selected, 2);
        if let Some(peer) = &peer {
            peer.close().unwrap();
        }
        drop(peer);
        engine.close().unwrap();
        drop(engine);
        assert!(
            memory.used() > 0,
            "the retained logical plan still owns its payload"
        );
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
}
