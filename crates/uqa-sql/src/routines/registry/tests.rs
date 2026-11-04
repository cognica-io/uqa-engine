//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBody, Statement},
    plan::UnifiedPlan,
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::CancellationToken;

fn routine(sql: &str) -> SQLUserFunction {
    let Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("routine")
    };
    let body = match &definition.body {
        FunctionBody::Source(_) => RoutineBody::Source,
        FunctionBody::Statements(statements) => RoutineBody::Bound(
            CompiledFunctionBody::SQL(statements.iter().cloned().map(UnifiedPlan::lower).collect())
                .shared(),
        ),
    };
    SQLUserFunction::new(*definition, body)
}

fn encoded(source: &CapturedRoutines) -> serde_json::Value {
    serde_json::to_value(source).unwrap()
}

#[test]
fn native_routines_preserve_exact_overloads_and_the_original_capture_encoding() {
    let mut entries = Vec::new();
    for (index, sql) in [
        "CREATE FUNCTION f(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL IMMUTABLE STRICT SECURITY DEFINER PARALLEL SAFE COST 42 SET search_path TO public RETURN a + 1",
        "CREATE FUNCTION f(a text) RETURNS text LANGUAGE SQL AS 'SELECT a'",
        "CREATE FUNCTION f() RETURNS integer LANGUAGE plpgsql AS 'unexamined source'",
        "CREATE PROCEDURE p() LANGUAGE SQL BEGIN ATOMIC INSERT INTO docs VALUES (1); END",
    ].into_iter().enumerate() {
        let mut function = routine(sql);
        function.def.object_id = Some([u8::try_from(index + 1).unwrap(); 16]);
        function.def.catalog_revision = Some([27; 16]);
        function.def.catalog_oid = Some(30000 + u32::try_from(index).unwrap());
        function.def.creation_search_path = vec!["Quoted.Schema".into(), "public".into()];
        function.def.execute_acl = Some(Vec::new());
        function.definition_version().unwrap();
        entries.push(function.shared());
    }
    let source = BTreeMap::from([
        ("\"public\".\"Quoted.Routine\"".into(), entries),
        ("public.empty".into(), Vec::new()),
        (
            "pg_temp_7.excluded".into(),
            vec![routine("CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 8").shared()],
        ),
    ]);
    let native = RoutineRegistry::try_from_iter(source.clone()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let captured = native.capture(&control).unwrap();
    let legacy = crate::catalog::definition_capture::routines::routines(&source, &control).unwrap();
    let expected = serde_json::Value::Array(
        legacy
            .iter()
            .map(|(name, overloads)| {
                serde_json::json!([
                    name,
                    overloads
                        .iter()
                        .map(|routine| serde_json::json!({
                            "definition": &routine.definition,
                            "bound_body": &routine.bound_body,
                        }))
                        .collect::<Vec<_>>()
                ])
            })
            .collect(),
    );
    assert_eq!(encoded(&captured), expected);
    assert_eq!(captured.len(), 2);
    for (name, overloads) in captured.iter() {
        for (index, (captured, original)) in overloads.iter().zip(&source[name]).enumerate() {
            assert_eq!(captured.catalog_revision(), original.catalog_revision());
            assert_eq!(
                captured.definition_version().unwrap(),
                original.definition_version().unwrap()
            );
            assert!(Arc::ptr_eq(captured, &native[name][index]));
        }
    }
    drop((captured, legacy));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_capture_has_constant_work_and_independent_exact_admission() {
    for count in [1, 64, 4096] {
        let mut native = RoutineRegistry::new();
        let function =
            routine("CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 7").shared();
        for index in 0..count {
            native
                .insert(format!("public.f_{index:04}"), vec![function.clone()])
                .unwrap();
        }
        native
            .insert("pg_temp_7.hidden".into(), vec![function])
            .unwrap();
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut captured = None;
        let allocation =
            allocation_counter::measure(|| captured = Some(native.capture(&control).unwrap()));
        assert_eq!(allocation.count_total, 0, "{count}: {allocation:?}");
        let captured = captured.unwrap();
        assert_eq!(captured.len(), count);
        let required = captured.reserved_bytes();
        assert_eq!(memory.used(), required);
        assert!(required > 0);
        let exact = MemoryBudget::new(required);
        let independent = native
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), required);
        let insufficient = MemoryBudget::new(required - 1);
        assert_eq!(
            native
                .capture(&ProductionControl::new(&insufficient, &token, &token))
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(insufficient.used(), 0);
        drop((native, captured));
        assert_eq!(memory.used(), 0);
        assert_eq!(independent.len(), count);
        drop(independent);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn selected_routine_publication_shares_unrelated_overloads_and_preserves_failed_generations() {
    let memory = MemoryBudget::new(4 << 20);
    let mut native = RoutineRegistry::with_budget(&memory);
    let small = routine("CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 7").shared();
    let mut large = routine("CREATE FUNCTION f(a text) RETURNS text LANGUAGE SQL AS 'unexamined'");
    large.def.body = FunctionBody::Source("x".repeat(1 << 20));
    native
        .insert("public.f".into(), vec![small, large.shared()])
        .unwrap();
    let original = native.clone();
    let previous_large = native["public.f"][1].clone();
    let next = routine("CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 9").shared();
    let allocation = allocation_counter::measure(|| {
        native
            .edit("public.f", |overloads| {
                overloads[0] = next;
                Ok(())
            })
            .unwrap()
            .unwrap();
    });
    assert!(
        allocation.bytes_total < 32768 && allocation.count_total < 256,
        "{allocation:?}"
    );
    assert!(Arc::ptr_eq(&previous_large, &native["public.f"][1]));
    assert!(!Arc::ptr_eq(
        &original["public.f"][0],
        &native["public.f"][0]
    ));
    let before = native.clone();
    let mut rejected = routine("CREATE FUNCTION f() RETURNS text LANGUAGE SQL AS 'unexamined'");
    rejected.def.body = FunctionBody::Source("y".repeat(4 << 20));
    let used = memory.used();
    assert_eq!(
        native
            .insert("public.f".into(), vec![rejected.shared()])
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), used);
    assert!(Arc::ptr_eq(&before["public.f"][0], &native["public.f"][0]));
    native.remove("public.f").unwrap();
    assert!(native.is_empty());
    assert_eq!(original.len(), 1);
    drop((native, original, before, previous_large));
    assert_eq!(memory.used(), 0);
}

#[test]
fn escaped_routine_and_bound_body_handles_keep_their_native_allowances_until_final_release() {
    let memory = MemoryBudget::new(1 << 20);
    let mut native = RoutineRegistry::with_budget(&memory);
    native
        .insert(
            "public.f".into(),
            vec![
                routine("CREATE FUNCTION f() RETURNS text LANGUAGE SQL RETURN 'retained body'")
                    .shared(),
            ],
        )
        .unwrap();
    let function = native["public.f"][0].clone();
    let RoutineBody::Bound(body) = &function.body else {
        panic!("bound")
    };
    let body = body.clone();
    let body_bytes = body.reserved_bytes();
    let function_bytes = function.reserved_bytes();
    assert!(body_bytes > 0 && function_bytes > 0);
    drop(native);
    assert_eq!(memory.used(), function_bytes + body_bytes);
    assert!(function.is_retained_by(&memory));
    drop(function);
    assert_eq!(memory.used(), body_bytes);
    let CompiledFunctionBody::SQL(plans) = &**body else {
        panic!("SQL")
    };
    assert_eq!(plans.len(), 1);
    drop(body);
    assert_eq!(memory.used(), 0);
}

#[test]
fn routine_capture_checks_both_cancellation_owners_without_changing_native_authority() {
    let mut native = RoutineRegistry::new();
    native
        .insert(
            "public.f".into(),
            vec![routine("CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 7").shared()],
        )
        .unwrap();
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let memory = MemoryBudget::new(1 << 20);
        assert_eq!(
            native
                .capture(&ProductionControl::new(&memory, &original, &invoking))
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        assert_eq!(native.len(), 1);
    }
}
