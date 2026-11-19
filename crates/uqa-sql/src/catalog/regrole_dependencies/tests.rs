//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Roles {
    reads: Cell<usize>,
}

impl StoredRegroleResolver for Roles {
    fn resolve_stored_regrole(&self, input: &str) -> Result<Option<i64>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        input.parse().map(Some).map_err(|_| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{input}\" does not exist"),
        })
    }
}

fn expression(sql: &str) -> Expr {
    let crate::Statement::Select(mut statement) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        unreachable!()
    };
    statement.projections.remove(0).expr
}

#[test]
fn converted_role_constants_keep_dependencies_without_rereading() {
    for oid in [0, 23] {
        let converted = Expr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value: Value::Int(oid),
            ty: "regrole".into(),
        };
        for stored in [
            converted.clone(),
            Expr::Cast {
                expr: Box::new(converted),
                ty: "oid".into(),
                implicit: true,
            },
        ] {
            let roles = Roles::default();
            let mut constants = StoredRegroleConstants::default();
            constants.collect_expression(&stored, None);
            let error = constants.reject_with(&roles).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                "constant of the type regrole cannot be used here"
            );
            assert_eq!(roles.reads.get(), 0, "bound inputs are not read again");
        }
    }
}

#[test]
fn raw_input_errors_precede_dependency_rejection() {
    let roles = Roles::default();
    let mut constants = StoredRegroleConstants::default();
    constants.collect_expression(
        &Expr::Literal(Value::Str("missing".into())),
        Some(&ColumnType::Regrole),
    );
    let error = constants.reject_with(&roles).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(roles.reads.get(), 1);
}

#[test]
fn runtime_numeric_null_and_array_role_expressions_remain_storable() {
    let roles = Roles::default();
    for sql in [
        "('23'::text)::regrole",
        "10::regrole",
        "NULL::regrole",
        "'{23}'::regrole[]",
    ] {
        let stored = expression(sql);
        let mut constants = StoredRegroleConstants::default();
        constants.collect_expression(&stored, None);
        constants
            .reject_with(&roles)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn query_dependency_collection_distinguishes_typed_null_and_role_constants() {
    for (value, rejected) in [(Value::Int(23), true), (Value::Null, false)] {
        let crate::Statement::Select(mut query) = crate::compile("SELECT 23").unwrap().remove(0)
        else {
            unreachable!()
        };
        query.projections[0].expr = Expr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value,
            ty: "regrole".into(),
        };
        let mut plan = QueryPlan::lower(*query);
        let mut constants = StoredRegroleConstants::default();
        constants.collect_query_plan(&mut plan);
        assert_eq!(constants.reject_with(&Roles::default()).is_err(), rejected);
    }
}

#[test]
fn stored_role_diagnostics_follow_mutation_and_query_input_order() {
    for (sql, expected) in [
        ("WITH c AS (SELECT 'cte'::regrole) SELECT 'projection'::regrole FROM (SELECT 'source'::regrole) s WHERE 'predicate'::regrole IS NOT NULL", &["cte", "source", "predicate", "projection"][..]),
        ("WITH c AS (SELECT 'cte'::regrole) INSERT INTO t(x[('target'::regrole)::integer]) SELECT 'source'::regrole ON CONFLICT (x) DO UPDATE SET x = 'conflict'::regrole RETURNING 'returning'::regrole", &["target", "cte", "source", "conflict", "returning"][..]),
        ("UPDATE t SET x = 'assignment'::regrole FROM (SELECT 'source'::regrole) s WHERE 'predicate'::regrole IS NOT NULL RETURNING 'returning'::regrole", &["source", "assignment", "predicate", "returning"][..]),
        ("DELETE FROM t USING (SELECT 'source'::regrole) s WHERE 'predicate'::regrole IS NOT NULL RETURNING 'returning'::regrole", &["source", "predicate", "returning"][..]),
        ("MERGE INTO t USING (SELECT 'source'::regrole AS x) s ON 'join'::regrole IS NOT NULL WHEN MATCHED THEN UPDATE SET x = 'assignment'::regrole", &["source", "join", "assignment"][..]),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut rewrite = StoredRegroleConstants::default();
        plan.rewrite_scalar_expressions(&mut |node| rewrite.collect_scalar(node));
        assert_eq!(rewrite.inputs, expected, "{sql}");
        let mut collected = StoredRegroleConstants::default();
        collected.collect_plan(&mut plan);
        assert_eq!(collected.inputs, expected, "{sql}");
        let roles = Roles::default();
        assert_eq!(collected.reject_with(&roles).unwrap_err().to_string(), format!("role \"{}\" does not exist", expected[0]));
        assert_eq!(roles.reads.get(), 1);
    }
}

#[test]
fn stored_role_collection_yields_inside_names_and_releases_original_allowance() {
    use std::sync::Arc;
    use uqa_core::{
        image_io::{ImageDecodeWindow, ImageDecoder},
        memory::MemoryBudget,
        CancellationToken,
    };

    let name = "識".repeat(16_384);
    let sql = format!("SELECT '23'::regrole, '{name}'::regrole");
    let plan = Arc::new(UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0)));
    let native = MemoryBudget::new(256 * 1024);
    let work = MemoryBudget::new(256 * 1024);
    let invoking = CancellationToken::new();
    let decode = |original: CancellationToken| {
        let plan = Arc::clone(&plan);
        let native = native.clone();
        ImageDecoder::new(&work, &invoking, |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(&native, &original, input.cancellation);
            StoredRegroleConstants::from_plan(plan.as_ref().into(), &control, &mut input).await
        })
        .unwrap()
    };
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    let mut decoder = decode(CancellationToken::new());
    let mut polls = 0;
    let constants = loop {
        polls += 1;
        if let Some(value) = decoder.advance(&mut std::io::empty(), window).unwrap() {
            break value;
        }
    };
    assert!(polls > name.len() / 4096);
    assert_eq!(constants.inputs, ["23", &name]);
    assert_eq!(native.used(), constants.reserved_bytes());
    let roles = Roles::default();
    let mut next = 0;
    assert!(constants.validate_next_input(&roles, &mut next).unwrap());
    assert_eq!((next, roles.reads.get()), (1, 1));
    assert_eq!(
        constants
            .validate_next_input(&roles, &mut next)
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    assert_eq!((next, roles.reads.get()), (1, 2));
    drop((constants, decoder));
    assert_eq!((native.used(), work.used()), (0, 0));

    let original = CancellationToken::new();
    let mut decoder = decode(original.clone());
    assert!(decoder
        .advance(&mut std::io::empty(), window)
        .unwrap()
        .is_none());
    original.cancel();
    assert_eq!(
        decoder
            .advance(&mut std::io::empty(), window)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(decoder);
    let held = native.reserve(native.available()).unwrap();
    let mut decoder = decode(CancellationToken::new());
    let error = loop {
        match decoder.advance(&mut std::io::empty(), window) {
            Ok(None) => (),
            Ok(Some(_)) => panic!("role text exceeded its allowance"),
            Err(error) => break error,
        }
    };
    assert!(error.native_resource_error().is_some());
    assert_eq!(error.sqlstate(), Some("53200"));
    drop((held, decoder));
    assert_eq!((native.used(), work.used()), (0, 0));
}
