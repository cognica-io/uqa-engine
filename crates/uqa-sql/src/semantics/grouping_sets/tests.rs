//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{RelationalPlan, UnifiedPlan};
use crate::routines::RoutineResolution;

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        if let Some(element) = name.strip_suffix("[]") {
            return self
                .resolve_type_name(element)
                .map(|element| element.map(|element| ColumnType::Array(Box::new(element))));
        }
        let name = name.replace('"', "");
        Ok(matches!(
            name.as_str(),
            "grouping_literal_numeric" | "public.grouping_literal_numeric"
        )
        .then(|| ColumnType::Domain {
            schema: "public".into(),
            name: "grouping_literal_numeric".into(),
            oid: 50_001,
            base: Box::new(ColumnType::Numeric {
                precision: None,
                scale: None,
            }),
        }))
    }

    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&crate::ast::FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for Catalog {}

impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _name: &str) -> bool {
        false
    }
}

fn block(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    *block
}

fn input() -> RowSchema {
    RowSchema::with_types(
        vec!["id".into(), "n".into()],
        vec![Some(ColumnType::Integer); 2],
    )
}

#[test]
fn grouped_literal_validation_matches_postgresql_before_evaluation() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../aggregates/pg18_literals.json")).unwrap();
    let schema = RowSchema::with_identities(
        vec!["n".into()],
        vec![crate::ColumnIdentity::qualified("t", "n")],
        vec![Some(ColumnType::Integer)],
    );
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        if sql.starts_with("PREPARE ") {
            continue;
        }
        let result = validate_grouped_expressions(&Catalog, &block(sql), &schema, &[]);
        match (case["sqlstate"].as_str(), result) {
            (None, Ok(())) => {}
            (Some(expected), Err(error))
                if error.sqlstate() == Some(expected)
                    && error.to_string() == case["message"].as_str().unwrap() => {}
            (expected, actual) => differences.push(format!(
                "{}: expected {expected:?}, got {actual:?}",
                case["name"]
            )),
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn grouping_input_names_precede_aggregate_and_scalar_output_aliases() {
    for sql in [
        "SELECT min(id) AS first_id, count(*) AS n FROM t GROUP BY n",
        "SELECT min(id) AS first_id, 0 AS n FROM t GROUP BY n",
    ] {
        let original = block(sql);
        assert!(prepare_grouping_sets(&Catalog, &original, &input(), &[])
            .unwrap()
            .is_none());
        assert!(matches!(original.group_by.as_slice(), [ScalarExpr::Column(name)] if name == "n"));
    }
}

#[test]
fn grouping_aliases_resolve_before_distinct_sets_and_storage() {
    let original = block("SELECT n + 1 AS shifted, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((shifted), (n + 1))");
    let prepared = prepare_grouping_sets(&Catalog, &original, &input(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(prepared.grouping_sets.len(), 1);
    assert_eq!(
        prepared.grouping_sets[0],
        [original.projections[0].expr.clone()]
    );
    let mut stored = original;
    assert!(bind_grouping_names(&Catalog, &mut stored, &input(), &[]).unwrap());
    assert!(!bind_grouping_names(&Catalog, &mut stored, &input(), &[]).unwrap());
    assert_eq!(stored.grouping_sets[0], stored.grouping_sets[1]);
}

#[test]
fn grouping_set_computed_keys_remain_whole_aggregate_dependencies() {
    let original = block("SELECT n + 1 AS shifted, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((shifted), (n + 1), ())");
    let prepared = prepare_grouping_sets(&Catalog, &original, &input(), &[])
        .unwrap()
        .unwrap();
    let output = crate::semantics::sets::rewrite::prepare_aggregate_output_projection(
        &Catalog,
        &prepared,
        &[],
    );
    assert_eq!(
        output.statement.projections[0].expr,
        original.projections[0].expr
    );
    assert!(matches!(output.projections[0].1, ScalarExpr::Position(0)));
    assert_eq!(output.statement.grouping_sets.len(), 2);
}

#[test]
fn grouping_duplicate_aliases_use_analyzed_expression_identity() {
    let identical = block("SELECT n AS x, n AS x FROM t GROUP BY x");
    assert!(prepare_grouping_sets(&Catalog, &identical, &input(), &[])
        .unwrap()
        .is_some());
    for sql in [
        "SELECT id AS x, n AS x FROM t GROUP BY x",
        "SELECT 1 AS x, 1.0 AS x GROUP BY x",
    ] {
        assert_eq!(
            prepare_grouping_sets(&Catalog, &block(sql), &input(), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42702"),
            "{sql}"
        );
    }
}

#[test]
fn grouping_aliases_retain_aggregate_and_window_context_errors() {
    for (sql, state) in [
        ("SELECT count(*) AS tally FROM t GROUP BY tally", "42803"),
        (
            "SELECT row_number() OVER () AS rank FROM t GROUP BY rank",
            "42P20",
        ),
    ] {
        assert_eq!(
            prepare_grouping_sets(&Catalog, &block(sql), &input(), &[])
                .unwrap_err()
                .sqlstate(),
            Some(state),
            "{sql}"
        );
    }
}

#[test]
fn grouping_names_inside_expressions_are_never_output_aliases() {
    let original = block("SELECT n + 1 AS shifted FROM t GROUP BY shifted + 1");
    assert!(prepare_grouping_sets(&Catalog, &original, &input(), &[])
        .unwrap()
        .is_none());
}

#[test]
fn grouping_identity_keeps_catalog_casts_for_the_catalog_resolver() {
    for ty in [
        "regclass",
        "regnamespace",
        "regrole",
        "regtype",
        "regproc",
        "regprocedure",
        "grouping_literal_numeric",
        "grouping_literal_numeric[]",
        "regtype[]",
    ] {
        let input = ScalarExpr::Literal(Value::Str("catalog-dependent input".into()));
        let expression = ScalarExpr::Cast {
            expr: Box::new(input.clone()),
            ty: ty.into(),
        };
        let normalized = normalize_expression(
            &Catalog,
            expression.clone(),
            &RowSchema::with_types(Vec::new(), Vec::new()),
            &[],
        )
        .unwrap();
        let ScalarExpr::Cast { expr, .. } = normalized else {
            panic!("{ty}: catalog input was evaluated during grouping analysis");
        };
        assert_eq!(*expr, input, "{ty}");
    }
}
