//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    binding::statements::{StatementAnalysisOperation, StatementAnalysisScopes},
    routines::RoutineResolution,
    schema::dependencies::oid_alias::OidAliasInput,
    FunctionTypeResolver,
};

struct Catalog;
impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for Catalog {}
impl StatementAnalysisScopes for Catalog {
    fn with_scope(&self, _: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        panic!("dependency-free fixture has no catalog freshness callbacks");
    }
}
impl OidAliasInput for Catalog {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        Ok(None)
    }
}

fn context() -> PreparedDefinitionContext<'static> {
    PreparedDefinitionContext {
        types: &Catalog,
        routines: &Catalog,
        scopes: &Catalog,
        aliases: &Catalog,
    }
}

fn definition() -> Result<PreparedDefinition, SQLError> {
    Ok(PreparedDefinition {
        composite_inputs: uqa_sql::prepared::composites::CompositeInputs::default(),
        logical_plan: uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile("SELECT 1")?.remove(0)),
        parameter_types: vec![],
        result_schema: None,
        effective_search_path: None,
        dependencies: uqa_sql::prepared::dependencies::PreparedAnalysisDependencies::default(),
        dependency_snapshot: None,
    })
}

#[test]
fn retained_inputs_separate_statement_positions_concrete_types_and_definition_versions() {
    let cache = SQLRoutineInputs::default();
    let original = SQLBodyIdentity {
        object: [1; 16],
        version: 1,
    };
    let first = cache
        .statement(original, 0, &[ColumnType::Integer], &context(), definition)
        .unwrap();
    let repeated = cache
        .statement(original, 0, &[ColumnType::Integer], &context(), || {
            panic!("input must be retained")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&first, &repeated));
    for (identity, position, ty) in [
        (original, 1, ColumnType::Integer),
        (original, 0, ColumnType::BigInteger),
        (
            SQLBodyIdentity {
                version: 2,
                ..original
            },
            0,
            ColumnType::Integer,
        ),
    ] {
        let fresh = cache
            .statement(identity, position, &[ty], &context(), definition)
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &fresh));
    }
    let restored = cache
        .statement(original, 0, &[ColumnType::Integer], &context(), definition)
        .unwrap();
    assert!(
        !Arc::ptr_eq(&first, &restored),
        "rollback restores the definition, not discarded private cache state"
    );
}

#[test]
fn failed_analysis_is_retried_and_recursive_preparation_does_not_hold_the_cache_lock() {
    let cache = SQLRoutineInputs::default();
    let identity = SQLBodyIdentity {
        object: [3; 16],
        version: 1,
    };
    assert!(cache
        .statement(identity, 0, &[], &context(), || Err(SQLError::Internal(
            "failed analysis".into()
        )))
        .is_err());
    let first = cache
        .statement(identity, 0, &[], &context(), || {
            cache.statement(identity, 1, &[], &context(), definition)?;
            definition()
        })
        .unwrap();
    let repeated = cache
        .statement(identity, 0, &[], &context(), || {
            panic!("successful analysis survives a later execution error")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&first, &repeated));
}

#[test]
fn publication_and_undo_events_invalidate_only_selected_analysis_dependencies() {
    use crate::statement::prepared::invalidation::{
        PreparedCatalogChange, PreparedInvalidationLog,
    };
    let bodies = crate::routines::invocation::bodies::SessionRoutineBodies::default();
    let cache = bodies.sql_inputs();
    let identity = SQLBodyIdentity {
        object: [5; 16],
        version: 1,
    };
    let prepare = || {
        let mut value = definition()?;
        value.dependencies.relations.insert(42);
        Ok(value)
    };
    let original = cache
        .statement(identity, 0, &[], &context(), prepare)
        .unwrap();
    PreparedCatalogChange::Relation(99).invalidate_with_routines(std::iter::empty(), &bodies);
    let unchanged = cache
        .statement(identity, 0, &[], &context(), || {
            panic!("unrelated relation")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&original, &unchanged));
    let mut log = PreparedInvalidationLog::default();
    log.record(PreparedCatalogChange::Relation(42));
    log.invalidate_with_routines(std::iter::empty(), &bodies);
    let changed = cache
        .statement(identity, 0, &[], &context(), prepare)
        .unwrap();
    assert!(!Arc::ptr_eq(&original, &changed));
    log.invalidate_with_routines(std::iter::empty(), &bodies);
    let restored = cache
        .statement(identity, 0, &[], &context(), prepare)
        .unwrap();
    assert!(
        !Arc::ptr_eq(&changed, &restored),
        "transaction completion resends the same publication event"
    );
}

fn selected(entry: &PreparedStatementPlan) -> uqa_sql::prepared::planning::PreparedPlanSelection {
    let plan = entry
        .plan
        .clone()
        .unwrap_or_else(|| (*entry.logical_plan).clone());
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
}

#[test]
fn successful_plan_selection_is_retained_but_failed_selection_does_not_change_usage() {
    let statement = SQLRoutineStatement::new(definition().unwrap());
    for used in 0..32 {
        statement
            .select(|entry| {
                assert_eq!(entry.generic_plans, used);
                assert_eq!(entry.plan.is_some(), used != 0);
                Ok(selected(entry))
            })
            .unwrap();
    }
    assert!(statement
        .select(|_| Err(SQLError::Internal("planning failed".into())))
        .is_err());
    let current = statement.variants.snapshot();
    assert_eq!(current.generic_plans, 32);
    assert!(current.plan.is_some());
}

#[test]
fn execution_invalidation_preserves_inputs_and_rejects_reentrant_plan_publication() {
    use crate::statement::prepared::invalidation::CatalogRegistryChange;
    let bodies = crate::routines::invocation::bodies::SessionRoutineBodies::default();
    let cache = bodies.sql_inputs();
    let identity = SQLBodyIdentity {
        object: [7; 16],
        version: 1,
    };
    let statement = cache
        .statement(identity, 0, &[], &context(), definition)
        .unwrap();
    statement.select(|entry| Ok(selected(entry))).unwrap();
    CatalogRegistryChange::BuiltinRoutinePrivileges
        .invalidate_with_routines(std::iter::empty(), &bodies);
    assert!(statement.variants.snapshot().plan.is_some());
    statement
        .select(|entry| {
            CatalogRegistryChange::Definitions
                .invalidate_with_routines(std::iter::empty(), &bodies);
            Ok(selected(entry))
        })
        .unwrap();
    let current = statement.variants.snapshot();
    assert!(current.plan.is_none());
    assert_eq!(current.generic_plans, 1);
    drop(current);
    let repeated = cache
        .statement(identity, 0, &[], &context(), || panic!("inputs survive"))
        .unwrap();
    assert!(Arc::ptr_eq(&statement, &repeated));
}
