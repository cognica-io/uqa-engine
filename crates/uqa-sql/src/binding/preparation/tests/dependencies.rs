//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::prepared::dependencies::PreparedAnalysisDependencies;
use uqa_core::Value;

mod fixtures;
use fixtures::{Aliases, Catalog, Routines};

fn analyze(sql: &str) -> (UnifiedPlan, PreparedAnalysisDependencies) {
    let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    let analysis = read_prepared_inputs(
        &Routines,
        &mut plan,
        &[],
        &Catalog::context(),
        Some(&Aliases),
    )
    .unwrap_or_else(|error| panic!("{sql}: {error}"));
    (plan, analysis.dependencies)
}

#[test]
fn dependencies_follow_bound_relations_through_views_ctes_and_write_targets() {
    for (sql, expected) in [
        ("SELECT id FROM base", vec![101]),
        ("WITH base AS (SELECT 1 AS id) SELECT id FROM base", vec![]),
        (
            "WITH base AS (SELECT 1 AS id) UPDATE base SET id=2 WHERE id=1",
            vec![101],
        ),
        ("SELECT (SELECT id FROM base)", vec![101]),
        ("SELECT id FROM outer_view", vec![101, 201, 202]),
        ("SELECT id FROM materialized", vec![203]),
    ] {
        let (_, dependencies) = analyze(sql);
        assert_eq!(
            dependencies.relations,
            expected.into_iter().collect(),
            "{sql}"
        );
    }
}

#[test]
fn dependencies_record_selected_overloads_and_stored_view_calls_without_routine_bodies() {
    for (sql, relations, routines) in [
        (
            "SELECT f(g(id)) FROM base",
            vec![101],
            vec![[7; 16], [9; 16]],
        ),
        ("SELECT f('value'::text)", vec![], vec![[8; 16]]),
        ("SELECT id FROM routine_view", vec![101, 204], vec![[7; 16]]),
    ] {
        let (_, dependencies) = analyze(sql);
        assert_eq!(
            dependencies.relations,
            relations.into_iter().collect(),
            "{sql}"
        );
        assert_eq!(
            dependencies.routines,
            routines.into_iter().collect(),
            "{sql}"
        );
        assert!(
            !dependencies.relations.contains(&102),
            "routine body is not parse analysis: {sql}"
        );
    }
}

#[test]
fn ordered_oid_inputs_freeze_scalar_and_array_values_but_only_scalar_relations_are_dependencies() {
    let (plan, dependencies) = analyze(
        "SELECT 'base'::regclass, '23'::oid, '{array_only}'::regclass[], ARRAY['array_item'::regclass], nextval('sequence_input')",
    );
    assert_eq!(dependencies.relations, BTreeSet::from([23, 101, 302, 303]));
    let mut frozen_array = false;
    plan.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Array(values),
                bound_type,
                ..
            } = expression
            {
                assert_eq!(
                    bound_type,
                    &Some(ColumnType::Array(Box::new(ColumnType::Regclass)))
                );
                assert_eq!(values.elements(), &[Value::Int(301)]);
                frozen_array = true;
            }
        });
    });
    assert!(frozen_array);
    let (_, runtime) = analyze("SELECT 'base'::text::regclass");
    assert!(
        runtime.relations.is_empty(),
        "typed text conversion is executable, not an input constant"
    );
}

#[test]
fn missing_oid_input_reports_its_analysis_error_before_a_later_expression() {
    for sql in [
        "SELECT 'absent'::regclass, missing_column",
        "SELECT nextval('absent'), missing_column",
        "SELECT currval('absent'), missing_column",
        "SELECT setval('absent', 1), missing_column",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = read_prepared_inputs(
            &Routines,
            &mut plan,
            &[],
            &Catalog::context(),
            Some(&Aliases),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"), "{sql}");
        assert_eq!(
            error.to_string(),
            "relation \"absent\" does not exist",
            "{sql}"
        );
    }
}

#[test]
fn sequence_signature_binding_keeps_catalog_inputs_and_volatile_calls_distinct() {
    for (name, types) in [
        ("nextval", vec![None]),
        ("currval", vec![None]),
        ("setval", vec![None, Some(ColumnType::Integer)]),
        (
            "setval",
            vec![None, Some(ColumnType::Integer), Some(ColumnType::Boolean)],
        ),
    ] {
        let selected = crate::resolve_fixed_builtin_call(
            name,
            None,
            &vec![None; types.len()],
            &types,
            false,
            Some(&Routines),
        )
        .unwrap()
        .unwrap();
        assert!(selected.builtin_non_immutable, "{name}");
        assert!(selected.selected.binding.builtin, "{name}");
        assert_eq!(selected.selected.binding.argument_types[0], "regclass");
    }
}

#[test]
fn prepared_oid_array_inputs_preserve_dimensions_and_nondefault_lower_bounds() {
    let (plan, dependencies) =
        analyze("SELECT '[0:1][3:4]={{base,NULL},{array_only,array_item}}'::regclass[]");
    assert!(dependencies.relations.is_empty());
    let mut count = 0;
    plan.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Array(array),
                ..
            } = expression
            {
                assert_eq!(array.dimensions(), [2, 2]);
                assert_eq!(array.lower_bounds(), [0, 3]);
                assert_eq!(array.element_at(&[0, 3]), Some(&Value::Int(101)));
                assert_eq!(array.element_at(&[0, 4]), Some(&Value::Null));
                assert_eq!(array.element_at(&[1, 3]), Some(&Value::Int(301)));
                assert_eq!(array.element_at(&[1, 4]), Some(&Value::Int(302)));
                count += 1;
            }
        });
    });
    assert_eq!(count, 1);
}

#[test]
fn cycle_mark_inputs_contribute_dependencies_after_their_original_leaves_are_rewritten() {
    let (_, dependencies) = analyze(
        "WITH RECURSIVE walk(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM walk WHERE n<2) CYCLE n SET seen TO regclass 'base' DEFAULT regclass 'array_item' USING path SELECT n FROM walk",
    );
    assert_eq!(dependencies.relations, BTreeSet::from([101, 302]));
}
