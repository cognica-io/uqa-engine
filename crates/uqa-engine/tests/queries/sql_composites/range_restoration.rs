//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Previously stored compound range predicates preserve independent comparison evaluations after reopening.

use uqa_engine::Engine;
use uqa_sql::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch, FunctionOrderSyntax},
    catalog::stored_view::StoredViewDefinition,
    ScalarExpr,
};
use uqa_storage::CatalogFacade;

fn legacy_pair(parts: &[ScalarExpr]) -> Option<ScalarExpr> {
    let [ScalarExpr::Binary { op, lhs, rhs: low }, ScalarExpr::Binary {
        op: upper,
        rhs: high,
        ..
    }] = parts
    else {
        return None;
    };
    let negated = match (op, upper) {
        (BinaryOp::GreaterEqual, BinaryOp::LessEqual) => false,
        (BinaryOp::Less, BinaryOp::Greater) => true,
        _ => return None,
    };
    let expression = ScalarExpr::Between {
        expr: lhs.clone(),
        low: low.clone(),
        high: high.clone(),
    };
    Some(if negated {
        ScalarExpr::Not(Box::new(expression))
    } else {
        expression
    })
}

fn legacy_symmetric(parts: &[ScalarExpr]) -> Option<ScalarExpr> {
    let [first, _] = parts else {
        return None;
    };
    let (first, negated) = match first {
        ScalarExpr::Not(inner) => (inner.as_ref(), true),
        other => (other, false),
    };
    let ScalarExpr::Between { expr, low, high } = first else {
        return None;
    };
    let binding = FunctionBinding::dispatched(FunctionDispatch::BetweenSymmetric);
    let expression = ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![*expr.clone(), *low.clone(), *high.clone()],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: FunctionOrderSyntax::Ordinary,
    };
    Some(if negated {
        ScalarExpr::Not(Box::new(expression))
    } else {
        expression
    })
}

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/range_predicate_restoration_oracle.expected.json"
    ))
    .unwrap()
}

fn setup(engine: &Engine) {
    let reference = reference();
    let setup = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "setup")
        .unwrap();
    engine.sql(setup["sql"].as_str().unwrap(), &[]).unwrap();
}

fn verify(engine: &Engine) {
    let mut observations = reference();
    observations["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str(), Some("version" | "setup" | "cleanup")));
    crate::pg18_oracle::verify(engine, &observations.to_string());
}

fn predecessor_views(catalog: &dyn CatalogFacade) {
    let mut views = 0;
    for mut row in catalog.load_views().unwrap() {
        let mut definition: StoredViewDefinition =
            serde_json::from_str(&row.definition_json).unwrap();
        let mut changed = false;
        definition.query.rewrite_scalar_expressions(&mut |node| {
            if let ScalarExpr::And(parts) | ScalarExpr::Or(parts) = node {
                if let Some(legacy) = legacy_pair(parts).or_else(|| legacy_symmetric(parts)) {
                    *node = legacy;
                    changed = true;
                }
            }
        });
        assert!(changed, "range view retained no predecessor form");
        uqa_sql::plan::subqueries::prune_query(&mut definition.query);
        row.definition_json = serde_json::to_string(&definition).unwrap();
        catalog.save_view(&row).unwrap();
        views += 1;
    }
    assert_eq!(views, 5);
}

fn stored_views(catalog: &dyn CatalogFacade) -> Vec<String> {
    let mut rows = catalog
        .load_views()
        .unwrap()
        .iter()
        .map(|row| format!("{row:?}"))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn assert_canonical(catalog: &dyn CatalogFacade) {
    for row in catalog.load_views().unwrap() {
        let mut definition: StoredViewDefinition =
            serde_json::from_str(&row.definition_json).unwrap();
        definition.query.rewrite_scalar_expressions(&mut |node| {
            assert!(!matches!(node, ScalarExpr::Between { .. }));
            if let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = node
            {
                assert_ne!(binding.dispatch, Some(FunctionDispatch::BetweenSymmetric));
            }
        });
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn legacy_range_views_keep_postgresql_evaluation_counts(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-ranges.db");
    let engine = super::addition::open(provider, &path);
    setup(&engine);
    verify(&engine);
    drop(engine);
    super::addition::restoration::catalog(provider, &path, predecessor_views);
    let mut durable = None;
    for _ in 0..2 {
        let engine = super::addition::open(provider, &path);
        verify(&engine);
        drop(engine);
        super::addition::restoration::catalog(provider, &path, |catalog| {
            assert_canonical(catalog);
            let current = stored_views(catalog);
            if let Some(previous) = &durable {
                assert_eq!(&current, previous);
            }
            durable = Some(current);
        });
    }
}

#[test]
fn fresh_range_views_keep_postgresql_evaluation_counts_in_memory() {
    let engine = Engine::new();
    setup(&engine);
    verify(&engine);
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn failed_range_migration_preserves_the_predecessor_views(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-rollback.db");
    let engine = super::addition::open(provider, &path);
    setup(&engine);
    drop(engine);
    let (rules, before) = super::addition::restoration::catalog(provider, &path, |catalog| {
        predecessor_views(catalog);
        let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
        let mut future: serde_json::Value = serde_json::from_str(&rules).unwrap();
        future["format_version"] = serde_json::json!(u32::MAX);
        catalog
            .set_metadata("sql_rules_json", &future.to_string())
            .unwrap();
        (rules, stored_views(catalog))
    });
    let result = match provider {
        1 => Engine::open(&path).map_err(|error| error.to_string()),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        _ => unreachable!(),
    };
    let Err(error) = result else {
        panic!("future rule format must reject initial restoration");
    };
    assert!(error.contains("newer than supported"), "{error}");
    super::addition::restoration::catalog(provider, &path, |catalog| {
        assert_eq!(stored_views(catalog), before);
        catalog.set_metadata("sql_rules_json", &rules).unwrap();
    });
    drop(super::addition::open(provider, &path));
    super::addition::restoration::catalog(provider, &path, assert_canonical);
}
