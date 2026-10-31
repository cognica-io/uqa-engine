//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
use super::*;

mod registry;
use crate::statement::prepared::invalidation::{PreparedCatalogChange, PreparedInvalidationLog};
use uqa_sql::{binding::statements::ProceduralPlanAnalysis, plan::UnifiedPlan};

use std::sync::atomic::{AtomicUsize, Ordering};
fn fragment() -> PreparedFragment {
    let syntax = uqa_sql::compile("SELECT 1").unwrap().remove(0);
    let analysis = ProceduralPlanAnalysis {
        composite_inputs: uqa_sql::prepared::composites::CompositeInputs::default(),
        result: uqa_sql::binding::statements::AnalyzedResult::Rows(vec![Some(
            uqa_sql::ColumnType::Integer,
        )]),
        dependencies: uqa_sql::prepared::dependencies::PreparedAnalysisDependencies::default(),
        effective_search_path: None,
        dependency_snapshot: None,
    };
    let definition = analysis
        .prepared_definition(&UnifiedPlan::lower(syntax.clone()), &[])
        .unwrap()
        .into_shared();
    PreparedFragment {
        source: Arc::new(PLpgSQLSource {
            query: Arc::from("SELECT 1"),
            mode: uqa_sql::plpgsql::PLpgSQLParseMode::Statement,
        }),
        simple_expression: true,
        syntax: Arc::new(syntax),
        analysis: FragmentAnalysis::from_definition(&definition),
        parameter_types: definition.parameter_types.clone(),
        variants: super::super::plans::RoutinePlanVariants::new(&definition),
        valid: AtomicBool::new(true),
        variables: ProductionControl::uncontrolled()
            .finish(Vec::new(), None)
            .unwrap(),
    }
}
#[test]
fn analysis_failure_retries_but_success_is_retained_after_execution_failure() {
    let cache = PLpgSQLPreparations::default();
    let attempts = AtomicUsize::new(0);
    for _ in 0..2 {
        assert!(cache
            .get_or_prepare(
                1,
                |_| Ok(true),
                |_| {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    Err(SQLError::Routine {
                        sqlstate: "22P02".into(),
                        message: "bad input".into(),
                    })
                }
            )
            .is_err());
    }
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    let prepared = cache
        .get_or_prepare(1, |_| Ok(true), |_| Ok(fragment()))
        .unwrap();
    // Runtime execution does not own or roll back this published preparation.
    let again = cache
        .get_or_prepare(1, |_| Ok(true), |_| panic!("must not parse again"))
        .unwrap();
    assert!(Arc::ptr_eq(&prepared, &again));
    assert!(!Arc::ptr_eq(
        &prepared,
        &cache
            .get_or_prepare(2, |_| Ok(true), |_| Ok(fragment()))
            .unwrap()
    ));
}
#[test]
fn recursive_preparation_does_not_hold_the_cache_lock_or_overwrite_a_published_site() {
    let cache = PLpgSQLPreparations::default();
    let outer = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |_| {
                let inner = cache.get_or_prepare(1, |_| Ok(true), |_| Ok(fragment()))?;
                assert_eq!(inner.variables.len(), 0);
                Ok(fragment())
            },
        )
        .unwrap();
    let again = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |_| panic!("prepared by the inner activation"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&outer, &again));
}

#[test]
fn failed_reanalysis_keeps_raw_syntax_without_resurrecting_stale_inputs() {
    let cache = PLpgSQLPreparations::default();
    let original = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |raw| {
                assert!(raw.is_none());
                Ok(fragment())
            },
        )
        .unwrap();
    for _ in 0..2 {
        let error = cache
            .get_or_prepare(
                1,
                |_| Ok(false),
                |raw| {
                    assert!(std::ptr::eq(raw.unwrap(), &raw const original.syntax));
                    Err(SQLError::Routine {
                        sqlstate: "22P02".into(),
                        message: "stale input".into(),
                    })
                },
            )
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
    let repaired = cache
        .get_or_prepare(
            1,
            |_| panic!("stale even when old namespace returns"),
            |raw| {
                assert!(std::ptr::eq(raw.unwrap(), &raw const original.syntax));
                Ok(fragment())
            },
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&original, &repaired));
    assert!(repaired.valid.load(Ordering::Acquire));
}

#[test]
fn catalog_publication_reaches_live_anonymous_owners_without_retaining_dead_activations() {
    let bodies = crate::routines::invocation::bodies::SessionRoutineBodies::default();
    let cache = bodies
        .procedural
        .register(&ProductionControl::uncontrolled())
        .unwrap();
    let original = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |_| {
                let mut value = fragment();
                let mut dependencies = (*value.analysis.dependencies).clone();
                dependencies.relations.insert(42);
                value.analysis.dependencies = dependencies.into();
                Ok(value)
            },
        )
        .unwrap();
    PreparedCatalogChange::Relation(99).invalidate_with_routines(std::iter::empty(), &bodies);
    assert!(original.valid.load(Ordering::Acquire));
    let mut log = PreparedInvalidationLog::default();
    log.record(PreparedCatalogChange::Relation(42));
    log.invalidate_with_routines(std::iter::empty(), &bodies);
    assert!(!original.valid.load(Ordering::Acquire));
    let repaired = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |raw| {
                assert!(std::ptr::eq(raw.unwrap(), &raw const original.syntax));
                let mut value = fragment();
                let mut dependencies = (*value.analysis.dependencies).clone();
                dependencies.relations.insert(42);
                value.analysis.dependencies = dependencies.into();
                Ok(value)
            },
        )
        .unwrap();
    log.invalidate_with_routines(std::iter::empty(), &bodies);
    assert!(
        !repaired.valid.load(Ordering::Acquire),
        "undo must resend publication"
    );
    let weak = Arc::downgrade(&cache);
    drop(cache);
    assert!(weak.upgrade().is_none());
    let next = bodies
        .procedural
        .register(&ProductionControl::uncontrolled())
        .unwrap();
    assert_eq!(bodies.procedural.owners.lock().len(), 1);
    drop(next);
    log.invalidate_with_routines(std::iter::empty(), &bodies);
    assert!(bodies.procedural.owners.lock().is_empty());
}

#[test]
fn invalidated_recursive_preparation_keeps_the_newer_published_analysis() {
    let cache = PLpgSQLPreparations::default();
    let old = cache
        .get_or_prepare(1, |_| Ok(true), |_| Ok(fragment()))
        .unwrap();
    cache.invalidate(&|_| true);
    let mut inner = None;
    let outer = cache
        .get_or_prepare(
            1,
            |_| Ok(true),
            |raw| {
                assert!(std::ptr::eq(raw.unwrap(), &raw const old.syntax));
                inner = Some(cache.get_or_prepare(1, |_| Ok(true), |_| Ok(fragment()))?);
                Ok(fragment())
            },
        )
        .unwrap();
    assert!(Arc::ptr_eq(&outer, inner.as_ref().unwrap()));
    assert!(!Arc::ptr_eq(&outer, &old));
}

#[test]
fn executable_invalidation_reaches_procedural_owners_and_fences_recursive_publication() {
    use crate::statement::prepared::invalidation::CatalogRegistryChange;
    let bodies = crate::routines::invocation::bodies::SessionRoutineBodies::default();
    let owner = bodies
        .procedural
        .register(&ProductionControl::uncontrolled())
        .unwrap();
    let prepared = owner
        .get_or_prepare(
            1,
            |_| Ok(true),
            |_| {
                let mut value = fragment();
                value.syntax = Arc::new(
                    uqa_sql::compile("SELECT catalog_value()")
                        .unwrap()
                        .remove(0),
                );
                let mut dependencies = (*value.analysis.dependencies).clone();
                dependencies.routines.insert([7; 16]);
                value.analysis.dependencies = dependencies.into();
                value.variants = super::super::plans::RoutinePlanVariants::new(
                    &uqa_sql::prepared::definition::PreparedDefinition {
                        composite_inputs: uqa_sql::prepared::composites::CompositeInputs::default(),
                        logical_plan: UnifiedPlan::lower((*value.syntax).clone()).into(),
                        parameter_types: value.parameter_types.clone(),
                        result_schema: None,
                        dependencies: value.analysis.dependencies.clone(),
                        effective_search_path: value.analysis.effective_search_path.clone(),
                        dependency_snapshot: value.analysis.dependency_snapshot.clone(),
                    },
                );
                Ok(value)
            },
        )
        .unwrap();
    let selected = |entry: &uqa_sql::prepared::entry::PreparedStatementPlan| {
        let plan = entry
            .plan
            .clone()
            .unwrap_or_else(|| entry.logical_plan.clone());
        uqa_sql::prepared::planning::PreparedPlanSelection {
            plan: plan.clone(),
            update: uqa_sql::prepared::planning::PreparedPlanUpdate {
                composite_inputs: uqa_sql::prepared::composites::CompositeInputs::default(),
                reanalyzed: None,
                generic_plan: Some(plan),
                generic_cost: Some(1.0),
                custom_cost: None,
            },
        }
    };
    prepared
        .variants
        .select(|entry| Ok(selected(entry)))
        .unwrap();
    assert!(prepared.variants.snapshot().plan.is_some());
    prepared
        .variants
        .select(|entry| {
            CatalogRegistryChange::Definitions
                .invalidate_with_routines(std::iter::empty(), &bodies);
            Ok(selected(entry))
        })
        .unwrap();
    assert!(
        prepared.valid.load(Ordering::Acquire),
        "input constants survive execution-only invalidation"
    );
    assert!(prepared.variants.snapshot().plan.is_none());
    assert_eq!(prepared.variants.snapshot().generic_plans, 1);
}
