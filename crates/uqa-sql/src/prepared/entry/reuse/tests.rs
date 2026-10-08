//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::RowSchema;
use std::sync::Arc;

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn entry(source: &str, executable: &str) -> PreparedStatementPlan {
    let source = Arc::new(plan(source));
    PreparedStatementPlan {
        composite_inputs: crate::prepared::composites::CompositeInputs::default(),
        source_plan: Arc::clone(&source),
        logical_plan: source,
        needs_analysis: false,
        effective_search_path: None,
        dependencies: crate::prepared::dependencies::PreparedAnalysisDependencies::default(),
        dependency_snapshot: None,
        plan: Some(plan(executable)),
        parameter_types: Vec::new(),
        result_schema: Some(RowSchema::with_types(
            vec!["value".into()],
            vec![Some(ColumnType::Text)],
        )),
        source_sql: None,
        prepared_at_micros: 0,
        from_sql: true,
        generic_plans: 1,
        custom_plans: 0,
        generic_cost: Some(1.0),
        total_custom_cost: 0.0,
    }
}

fn domain() -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "guarded".into(),
        oid: 20_001,
        array_oid: Some(20_002),
        base: Box::new(ColumnType::Text),
    }
}

#[test]
fn folded_builtin_constants_are_independent_but_unbound_calls_are_not() {
    // PostgreSQL 18.4's revoke_cached reference retains this already-folded result after REVOKE; generic calls with live parameters still need initialization.
    let constant = entry("SELECT lower('HELLO') AS value", "SELECT 'hello' AS value");
    assert!(constant.has_tracked_executable_dependencies());
    for (source, executable) in [
        ("VALUES (lower('HELLO'))", "VALUES ('hello')"),
        (
            "SELECT lower('HELLO') UNION ALL SELECT 'goodbye'",
            "SELECT 'hello' UNION ALL SELECT 'goodbye'",
        ),
    ] {
        assert!(entry(source, executable).has_tracked_executable_dependencies());
    }
    let mut live = entry("SELECT lower($1) AS value", "SELECT lower($1) AS value");
    live.parameter_types = vec![Some(ColumnType::Text)];
    assert!(!live.has_tracked_executable_dependencies());
    let mut missing = constant.clone();
    missing.plan = None;
    assert!(!missing.has_tracked_executable_dependencies());
    let mut invalidated = constant;
    invalidated.needs_analysis = true;
    assert!(!invalidated.has_tracked_executable_dependencies());
}

#[test]
fn catalog_descriptors_and_input_identities_keep_conservative_invalidation() {
    for ty in [
        domain(),
        ColumnType::Array(Box::new(domain())),
        ColumnType::Regclass,
        ColumnType::Regprocedure,
        ColumnType::Record,
        ColumnType::Timestamp,
        ColumnType::Named("public.int4".into()),
    ] {
        let mut parameter = entry("SELECT 'hello' AS value", "SELECT 'hello' AS value");
        parameter.parameter_types = vec![Some(ty.clone())];
        assert!(!parameter.has_tracked_executable_dependencies(), "{ty:?}");
        let mut result = entry("SELECT 'hello' AS value", "SELECT 'hello' AS value");
        result.result_schema = Some(RowSchema::with_types(vec!["value".into()], vec![Some(ty)]));
        assert!(!result.has_tracked_executable_dependencies());
    }
    for source in [
        "SELECT 7::regclass::text AS value",
        "SELECT '{}'::regtype[]::text AS value",
        "SELECT 'now'::timestamp::text AS value",
        "SELECT 'x'::public.guarded::text AS value",
    ] {
        assert!(
            !entry(source, "SELECT 'hello' AS value").has_tracked_executable_dependencies(),
            "{source}"
        );
    }
    let mut converted = entry("SELECT lower('HELLO') AS value", "SELECT 'hello' AS value");
    Arc::make_mut(&mut converted.logical_plan).rewrite_scalar_expressions(&mut |expression| {
        if matches!(expression, ScalarExpr::Literal(_)) {
            *expression = ScalarExpr::TypedLiteral {
                composite_source: None,
                value: uqa_core::Value::Str("HELLO".into()),
                ty: "text".into(),
                bound_type: Some(domain()),
                parameter_index: None,
            };
        }
    });
    assert!(!converted.has_tracked_executable_dependencies());
}

#[test]
fn selected_objects_and_relation_structure_are_not_hidden_by_constant_outputs() {
    for (relations, routines) in [(vec![42], vec![]), (vec![], vec![[7; 16]])] {
        let mut value = entry("SELECT 'hello' AS value", "SELECT 'hello' AS value");
        value.dependencies.relations.extend(relations);
        value.dependencies.routines.extend(routines);
        assert!(!value.has_tracked_executable_dependencies());
    }
    for source in [
        "SELECT 'hello' AS value FROM source_table",
        "SELECT (SELECT 'hello') AS value",
        "WITH unused AS (SELECT 1) SELECT 'hello' AS value",
        "SELECT 'hello' AS value WINDOW unused AS (ORDER BY 1)",
    ] {
        assert!(
            !entry(source, "SELECT 'hello' AS value").has_tracked_executable_dependencies(),
            "{source}"
        );
    }
    let mut record = entry("SELECT 'hello' AS value", "SELECT 'hello' AS value");
    record.result_schema = record
        .result_schema
        .take()
        .map(|schema| schema.with_record_fields([(0, Arc::from([Some(ColumnType::Integer)]))]));
    assert!(!record.has_tracked_executable_dependencies());
}

fn mixed_entry() -> PreparedStatementPlan {
    let mut mixed = entry(
        "SELECT lower('HELLO') AS folded, upper($1) AS live",
        "SELECT 'hello' AS folded, upper($1) AS live",
    );
    mixed.parameter_types = vec![Some(ColumnType::Text)];
    mixed.result_schema = Some(RowSchema::with_types(
        vec!["folded".into(), "live".into()],
        vec![Some(ColumnType::Text), Some(ColumnType::Text)],
    ));
    mixed
        .plan
        .as_mut()
        .unwrap()
        .rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { binding, .. } = expression {
                *binding = Some(FunctionBinding {
                    name: "pg_catalog.upper".into(),
                    argument_types: vec!["text".into()],
                    builtin: true,
                    object_id: None,
                    dispatch: None,
                    invocation: None,
                    composite_field: None,
                    resolution_error: None,
                });
            }
        });
    mixed
}

#[test]
fn mixed_constants_and_exact_builtin_calls_keep_one_executable() {
    // Independent PostgreSQL 18.4 peer_acl_mixed capture: lower stays folded after its ACL is revoked, while surviving upper($1) checks the current ACL on every execution.
    let mixed = mixed_entry();
    assert!(mixed.has_tracked_executable_dependencies());
    for (name, argument_type, builtin, object_id) in [
        ("upper", "text", true, None),
        ("public.upper", "text", true, None),
        ("pg_catalog.upper", "integer", true, None),
        ("pg_catalog.upper", "regclass", true, None),
        ("pg_catalog.upper", "text", false, None),
        ("pg_catalog.upper", "text", true, Some([7; 16])),
    ] {
        let mut changed = mixed.clone();
        changed
            .plan
            .as_mut()
            .unwrap()
            .rewrite_scalar_expressions(&mut |expression| {
                if let ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } = expression
                {
                    binding.name = name.into();
                    binding.argument_types = vec![argument_type.into()];
                    binding.builtin = builtin;
                    binding.object_id = object_id;
                }
            });
        assert!(
            !changed.has_tracked_executable_dependencies(),
            "{name}({argument_type})"
        );
    }
    let mut unbound = mixed;
    unbound
        .plan
        .as_mut()
        .unwrap()
        .rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { binding, .. } = expression {
                *binding = None;
            }
        });
    assert!(!unbound.has_tracked_executable_dependencies());
}

#[test]
fn missing_and_specialized_descriptors_are_not_catalog_independent() {
    let mut missing = mixed_entry();
    missing.parameter_types[0] = None;
    assert!(!missing.has_tracked_executable_dependencies());
    missing.parameter_types[0] = Some(ColumnType::Text);
    missing.result_schema = Some(RowSchema::with_types(vec!["value".into()], vec![None]));
    assert!(!missing.has_tracked_executable_dependencies());
    let mut specialized = mixed_entry();
    specialized
        .plan
        .as_mut()
        .unwrap()
        .rewrite_scalar_expressions(&mut |expression| {
            if matches!(expression, ScalarExpr::Param(_)) {
                *expression = ScalarExpr::TypedLiteral {
                    composite_source: None,
                    value: uqa_core::Value::Str("first".into()),
                    ty: "text".into(),
                    bound_type: Some(ColumnType::Text),
                    parameter_index: Some(1),
                };
            }
        });
    assert!(!specialized.has_tracked_executable_dependencies());
}

#[test]
fn relation_executables_require_complete_exact_analysis_revisions() {
    use crate::prepared::dependencies::{PreparedDependencyRevision, PreparedDependencySnapshot};
    let mut relation = entry(
        "SELECT lower('HELLO') AS value FROM source_table",
        "SELECT 'hello' AS value FROM source_table",
    );
    relation.dependencies.relations.insert(42);
    assert!(!relation.has_tracked_executable_dependencies());
    relation.dependency_snapshot = Some(PreparedDependencySnapshot::default());
    assert!(!relation.has_tracked_executable_dependencies());
    let snapshot = relation.dependency_snapshot.as_mut().unwrap();
    snapshot.global_catalog = Some(PreparedDependencyRevision::new(1_u64));
    snapshot.relations.insert(42, None);
    assert!(!relation.has_tracked_executable_dependencies());
    relation
        .dependency_snapshot
        .as_mut()
        .unwrap()
        .relations
        .insert(42, Some(PreparedDependencyRevision::new(2_u64)));
    assert!(relation.has_tracked_executable_dependencies());
    relation.dependencies.relations.insert(43);
    assert!(!relation.has_tracked_executable_dependencies());
}
