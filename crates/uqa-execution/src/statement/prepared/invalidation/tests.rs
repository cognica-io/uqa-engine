//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn relation(oid: u32) -> PreparedAnalysisDependencies {
    PreparedAnalysisDependencies {
        relations: BTreeSet::from([oid]),
        ..Default::default()
    }
}

#[test]
fn catalog_events_match_only_selected_objects_until_global_invalidation() {
    let dependencies = PreparedAnalysisDependencies {
        relations: BTreeSet::from([10]),
        routines: BTreeSet::from([[7; 16]]),
    };
    assert!(PreparedCatalogChange::Relation(10).affects(&dependencies));
    assert!(!PreparedCatalogChange::Relation(11).affects(&dependencies));
    assert!(PreparedCatalogChange::Routine([7; 16]).affects(&dependencies));
    assert!(!PreparedCatalogChange::Routine([8; 16]).affects(&dependencies));
    let mut log = PreparedInvalidationLog::default();
    assert!(!log.affects(&dependencies));
    log.record(PreparedCatalogChange::Relation(11));
    assert!(!log.affects(&dependencies));
    log.record(PreparedCatalogChange::GlobalCatalog);
    assert!(log.affects(&dependencies));
    assert!(log.affects(&PreparedAnalysisDependencies::default()));
    log.record(PreparedCatalogChange::Routine([8; 16]));
    assert_eq!(log.levels[0].len(), 1);
}

#[test]
fn rollback_replays_undone_changes_without_retaining_them_for_outer_commit() {
    let mut log = PreparedInvalidationLog::default();
    log.record(PreparedCatalogChange::Relation(10));
    let mark = log.mark();
    log.record(PreparedCatalogChange::Relation(10));
    log.record(PreparedCatalogChange::Relation(20));
    let nested = log.mark();
    log.record(PreparedCatalogChange::Relation(30));
    log.release(nested);
    let undone = log.rollback_to(mark);
    for oid in [10, 20, 30] {
        assert!(undone.affects(&relation(oid)));
    }
    assert!(log.affects(&relation(10)));
    assert!(!log.affects(&relation(20)));
    assert!(!log.affects(&relation(30)));
    assert!(!log.rollback_to(mark).affects(&relation(10)));
    log.record(PreparedCatalogChange::Relation(40));
    log.release(mark);
    assert!(log.affects(&relation(40)));
}

#[test]
fn nested_transaction_commit_merges_events_into_the_current_parent_savepoint() {
    let mut parent = PreparedInvalidationLog::default();
    parent.record(PreparedCatalogChange::Relation(10));
    let mark = parent.mark();
    let mut child = PreparedInvalidationLog::default();
    child.record(PreparedCatalogChange::Relation(20));
    let _child_mark = child.mark();
    child.record(PreparedCatalogChange::GlobalCatalog);
    parent.append(child);
    assert!(parent.affects(&relation(30)));
    assert!(parent.rollback_to(mark).affects(&relation(30)));
    assert!(parent.affects(&relation(10)));
    assert!(!parent.affects(&relation(20)));
}

fn cached_entry(executable: &str) -> PreparedStatementPlan {
    use std::sync::Arc;
    use uqa_sql::{plan::UnifiedPlan, ColumnType, RowSchema};
    let lower = |sql: &str| UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
    let source = Arc::new(lower("SELECT lower('HELLO') AS value"));
    PreparedStatementPlan {
        composite_inputs: uqa_sql::prepared::composites::CompositeInputs::default(),
        source_plan: Arc::clone(&source),
        logical_plan: source,
        needs_analysis: false,
        effective_search_path: None,
        dependencies: uqa_sql::prepared::dependencies::PreparedAnalysisDependencies::default(),
        dependency_snapshot: None,
        plan: Some(lower(executable)),
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

#[test]
fn local_acl_publication_and_peer_refresh_preserve_mixed_builtin_plans() {
    use uqa_sql::{ast::FunctionBinding, ColumnType, RowSchema, ScalarExpr};
    let mut constant = cached_entry("SELECT 'hello' AS value");
    let mut unbound = cached_entry("SELECT lower('HELLO') AS value");
    let mut mixed = cached_entry("SELECT 'hello' AS folded, upper($1) AS live");
    let source = std::sync::Arc::new(uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT lower('HELLO') AS folded, upper($1) AS live")
            .unwrap()
            .remove(0),
    ));
    mixed.source_plan = std::sync::Arc::clone(&source);
    mixed.logical_plan = source;
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
    CatalogRegistryChange::BuiltinRoutinePrivileges.invalidate([
        &mut constant,
        &mut unbound,
        &mut mixed,
    ]);
    assert!(constant.plan.is_some());
    assert!(unbound.plan.is_some());
    assert!(mixed.plan.is_some());
    // Every local/peer registry refresh uses this conservative invalidator. The pinned upper call remains available for current-ACL initialization without exposing the folded lower call to a new plan.
    invalidate_execution_plans([&mut constant, &mut unbound, &mut mixed]);
    assert!(constant.plan.is_some());
    assert!(unbound.plan.is_none());
    assert!(mixed.plan.is_some());
    assert!(!constant.needs_analysis);
    assert!(!unbound.needs_analysis);
    assert!(!mixed.needs_analysis);
}

#[test]
fn definition_events_still_discard_referenced_plans_and_analysis_invalidations_win() {
    let mut referenced = cached_entry("SELECT 'hello' AS value");
    referenced.dependencies = relation(42);
    CatalogRegistryChange::Definitions.invalidate([&mut referenced]);
    assert!(referenced.plan.is_none());
    let mut constant = cached_entry("SELECT 'hello' AS value");
    PreparedCatalogChange::GlobalCatalog.invalidate([&mut constant]);
    assert!(constant.plan.is_none());
    assert!(constant.needs_analysis);
}
