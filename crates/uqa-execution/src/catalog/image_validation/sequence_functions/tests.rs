//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, Statement, TableConstraintSet},
    routines::{RoutineBody, SQLUserFunction},
};

fn catalog(argument: &str) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    let Statement::CreateTable(table) = uqa_sql::compile(&format!(
        "CREATE TABLE items(value bigint DEFAULT nextval({argument}))"
    ))
    .unwrap()
    .remove(0) else {
        panic!("table")
    };
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], table.columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(snapshot)
}

fn check(catalog: &CatalogReadView) -> Result<(), String> {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let result = validate(catalog, &ProductionControl::new(&memory, &token, &token))
        .map_err(|error| error.to_string());
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_sequence_function_names_require_relations_without_evaluating_runtime_inputs() {
    for argument in [
        "'public.items'",
        "'pg_catalog.pg_class'::regclass",
        "2147483647::regclass",
        "'public.absent'::text",
        "'2147483647'",
        "'-'",
        "'-'::regclass",
        "'010'",
        "'4294967295'",
    ] {
        check(&catalog(argument)).unwrap();
    }
    for argument in [
        "'public.absent'",
        "'public.absent'::regclass",
        "'unqualified'",
        "'+5'",
        "'08'",
        "'4294967296'",
    ] {
        assert!(check(&catalog(argument)).is_err(), "{argument}");
    }
}

#[test]
fn native_sequence_function_references_cover_bound_plans_and_preserve_source_bodies() {
    for bound in [false, true] {
        let mut snapshot = empty_catalog().snapshot().clone();
        snapshot.tables.insert(
            RelationIdentity::new("public", "items"),
            table_snapshot(
                [1; 16],
                vec![ColumnDef::nullable("id", uqa_sql::ColumnType::Integer)],
                TableConstraintSet::default(),
            ),
        );
        let Statement::CreateFunction(definition) = uqa_sql::compile("CREATE FUNCTION public.lazy() RETURNS bigint LANGUAGE SQL AS 'SELECT nextval(''public.absent'')'").unwrap().remove(0) else { panic!("function") };
        let body = if bound {
            let plan = UnifiedPlan::lower(
                uqa_sql::compile("SELECT nextval('public.absent')")
                    .unwrap()
                    .remove(0),
            );
            RoutineBody::Bound(uqa_sql::routines::CompiledFunctionBody::SQL(vec![plan]).shared())
        } else {
            RoutineBody::Source
        };
        RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(
                "public.lazy".into(),
                vec![SQLUserFunction::new(*definition, body).shared()],
            )
            .unwrap();
        assert_eq!(check(&CatalogReadView::new(snapshot)).is_err(), bound);
    }
}

#[test]
fn original_routine_sequence_arguments_precede_bound_plans_after_a_utility_statement() {
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
    use uqa_sql::ast::FunctionBody;

    let statement = |sql: &str| uqa_sql::compile(sql).unwrap().remove(0);
    let Statement::CreateFunction(mut definition) =
        statement("CREATE FUNCTION public.f() RETURNS bigint LANGUAGE SQL AS 'SELECT 1'")
    else {
        unreachable!("function")
    };
    definition.body = FunctionBody::Statements(vec![
        statement("NOTIFY checked, 'result'"),
        statement("SELECT nextval('unqualified')"),
    ]);
    let compiled = UnifiedPlan::lower(statement("SELECT nextval('public.absent')"));
    let body =
        RoutineBody::Bound(uqa_sql::routines::CompiledFunctionBody::SQL(vec![compiled]).shared());
    let mut snapshot = empty_catalog().snapshot().clone();
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert(
            "public.f".into(),
            vec![SQLUserFunction::new(*definition, body).shared()],
        )
        .unwrap();
    let view = CatalogReadView::new(snapshot);
    let ordinary = check(&view).unwrap_err();
    assert!(
        ordinary.contains("non-canonical relation reference `unqualified`"),
        "{ordinary}"
    );
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let error = {
        let mut reader = ImageDecoder::new(&memory, &token, move |mut reader| async move {
            let mut input = reader.input();
            let control =
                ProductionControl::new(input.memory, input.cancellation, input.cancellation);
            validate_with_work(&view, &control, &mut input).await
        })
        .unwrap();
        loop {
            match reader.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 3 },
            ) {
                Ok(None) => {}
                Ok(Some(())) => panic!("accepted the original non-canonical reference"),
                Err(error) => break error.to_string(),
            }
        }
    };
    assert_eq!(error, ordinary);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_sequence_function_validation_releases_denied_and_cancelled_workspace() {
    let catalog = catalog("'public.items'");
    let memory = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    assert_eq!(
        validate(&catalog, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            validate(&catalog, &control).unwrap_err().sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_function_validation_suspends_in_a_single_reference_and_releases_interrupted_work() {
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

    // A digit string with arbitrarily many leading zeros remains a runtime OID. A named reference must decode its quoted UTF-8 and use the original catalog.
    let numeric = catalog(&format!("'{}17'", "0".repeat(192 << 10)));
    let local = "含".repeat(64 << 10);
    let named = catalog(&format!("'public.\"{local}\"'"));
    let mut reached = Vec::new();
    for (view, numeric) in [(&numeric, true), (&named, false)] {
        for interruption in [None, Some("57014"), Some("53200")] {
            // Numeric OID recognition has no temporary string allocation to deny.
            if numeric && interruption == Some("53200") {
                continue;
            }
            let memory = MemoryBudget::new(4 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let token = original.clone();
            let view = view.clone();
            let mut reader = ImageDecoder::new(&memory, &invoking, move |mut reader| async move {
                let mut input = reader.input();
                let control = ProductionControl::new(input.memory, &token, input.cancellation);
                validate_with_work(&view, &control, &mut input).await
            })
            .unwrap();
            let window = ImageDecodeWindow { bytes: 1, steps: 1 };
            for _ in 0..20 {
                assert!(reader
                    .advance(&mut std::io::empty(), window)
                    .unwrap()
                    .is_none());
            }
            let held = match interruption {
                Some("57014") => {
                    original.cancel();
                    None
                }
                Some("53200") => Some(memory.reserve(memory.available()).unwrap()),
                _ => None,
            };
            let (step, result) = (20..100_000)
                .find_map(|step| match reader.advance(&mut std::io::empty(), window) {
                    Ok(None) => None,
                    Ok(Some(())) => Some((step, Ok(()))),
                    Err(error) => Some((step, Err(error))),
                })
                .expect("terminal sequence reference validation");
            if let Some(expected) = interruption {
                assert_eq!(result.unwrap_err().sqlstate(), Some(expected));
            } else {
                reached.push(step);
                if numeric {
                    result.unwrap();
                } else {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("missing native sequence function relation"));
                }
            }
            drop(reader);
            drop(held);
            assert_eq!(memory.used(), 0);
        }
    }
    assert_eq!(reached.len(), 2);
    assert!(reached.iter().all(|steps| *steps >= (192 << 10) / 4096));
}
