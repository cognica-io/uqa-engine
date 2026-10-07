//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::{Cell, RefCell};
use uqa_core::Value;
use uqa_sql::{
    binding::statements::{
        StatementAnalysisOperation, StatementAnalysisScopes, StatementBindingScope,
    },
    ColumnType, RowSchema, ScalarExpr,
};
mod fixtures;
use fixtures::NoRoutines;

struct Inputs {
    entry: RefCell<Option<PreparedStatementPlan>>,
    mode: &'static str,
    events: RefCell<Vec<&'static str>>,
    search_path: RefCell<Vec<String>>,
    fail_binding: Cell<bool>,
}

impl Default for Inputs {
    fn default() -> Self {
        let logical_plan = Arc::new(UnifiedPlan::lower(
            uqa_sql::compile("SELECT $1::integer AS value")
                .unwrap()
                .remove(0),
        ));
        Self {
            entry: RefCell::new(Some(PreparedStatementPlan {
                source_plan: logical_plan.clone(),
                logical_plan,
                needs_analysis: false,
                effective_search_path: None,
                dependencies:
                    uqa_sql::prepared::dependencies::PreparedAnalysisDependencies::default(),
                dependency_snapshot: None,
                plan: None,
                parameter_types: vec![Some(ColumnType::Integer)],
                result_schema: Some(RowSchema::with_types(
                    vec!["value".into()],
                    vec![Some(ColumnType::Integer)],
                )),
                source_sql: None,
                prepared_at_micros: 0,
                from_sql: false,
                generic_plans: 0,
                custom_plans: 5,
                generic_cost: None,
                total_custom_cost: 5.0,
            })),
            mode: "auto",
            events: RefCell::new(Vec::new()),
            search_path: RefCell::new(vec!["public".into()]),
            fail_binding: Cell::new(false),
        }
    }
}
impl Inputs {
    fn context(&self) -> PreparedPlanningContext<'_> {
        PreparedPlanningContext {
            session: self,
            analysis: PreparedDefinitionContext {
                types: &NoRoutines,
                routines: &NoRoutines,
                scopes: self,
                aliases: &NoRoutines,
            },
            optimization: self,
        }
    }
    fn record(&self, event: &'static str) {
        self.events.borrow_mut().push(event);
    }
    fn select(&self) -> Result<Option<UnifiedPlan>, SQLError> {
        select_plan(
            &self.context(),
            "saved",
            &[SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer)],
        )
    }

    fn prepare_temporal_input(&self) {
        let sql = "SELECT 'now'::timestamp AS value, $1::integer AS parameter";
        let source = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        let _clock = uqa_sql::expr::TransactionClockScope::enter(90_123_456_789);
        let definition = uqa_sql::prepared::definition::analyze_definition(
            &self.context().analysis,
            source.clone(),
            &[],
        )
        .unwrap();
        let mut entry = self.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        entry.source_plan = Arc::new(source);
        entry.logical_plan = Arc::new(definition.logical_plan);
        entry.effective_search_path = definition.effective_search_path;
        entry.parameter_types = definition.parameter_types;
        entry.result_schema = definition.result_schema;
        entry.source_sql = Some(Arc::from(sql));
        entry.prepared_at_micros = 42;
        entry.from_sql = true;
        entry.plan = Some((*entry.logical_plan).clone());
        entry.generic_cost = Some(1.0);
        self.events.borrow_mut().clear();
    }
}
fn has_parameter(plan: &UnifiedPlan) -> bool {
    let mut found = false;
    plan.clone().rewrite_scalar_expressions(&mut |expression| {
        found |= matches!(expression, ScalarExpr::Param(_));
    });
    found
}
fn temporal_inputs(plan: &UnifiedPlan) -> Vec<i64> {
    let mut values = Vec::new();
    plan.clone().rewrite_scalar_expressions(&mut |expression| {
        if let ScalarExpr::TypedLiteral {
            value: Value::Temporal(uqa_core::TemporalValue::Timestamp { micros }),
            ..
        } = expression
        {
            values.push(*micros);
        }
    });
    values
}
impl PreparedPlanSession for Inputs {
    fn prepared_entry(&self, name: &str) -> Option<PreparedStatementPlan> {
        assert_eq!(name, "saved");
        self.record("lookup");
        self.entry.borrow().clone()
    }
    fn plan_cache_mode(&self) -> Result<String, SQLError> {
        self.record("mode");
        Ok(self.mode.into())
    }
    fn publish_usage(
        &self,
        name: &str,
        logical_plan: &Arc<UnifiedPlan>,
        update: PreparedPlanUpdate,
    ) {
        assert_eq!(name, "saved");
        self.record("publish");
        let mut entry = self.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        assert!(Arc::ptr_eq(&entry.logical_plan, logical_plan));
        entry.record_execution(update);
    }
}
impl PreparedPlanOptimization for Inputs {
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError> {
        self.record(if has_parameter(&plan) {
            "optimize.generic"
        } else {
            "optimize.custom"
        });
        Ok(plan)
    }
    fn estimate_plan(&self, plan: &UnifiedPlan) -> Result<crate::plan_cost::PlanCost, SQLError> {
        let generic = has_parameter(plan);
        self.record(if generic {
            "cost.generic"
        } else {
            "cost.custom"
        });
        Ok(crate::plan_cost::PlanCost {
            rows: 1.0,
            execution: if generic { 10_000.0 } else { 1.0 },
            relations: 0,
        })
    }
}
impl StatementAnalysisScopes for Inputs {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        self.record("analysis");
        analyze(self)
    }
}
impl StatementBindingScope for Inputs {
    fn binding_context(&self) -> Result<uqa_sql::binding::context::BindingContext<'_>, SQLError> {
        self.record("binding");
        if self.fail_binding.get() {
            return Err(SQLError::Internal("namespace unavailable".into()));
        }
        let mut binding = fixtures::binding_context();
        binding.resolution.search_path = self.search_path.borrow().clone();
        Ok(binding)
    }
}

#[test]
fn routine_owned_definition_selects_once_without_the_named_registry() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    let mut entry = inputs.entry.borrow().as_ref().unwrap().clone();
    for value in 1..=32 {
        let selected = select_entry(
            &inputs.context(),
            &entry,
            &[SQLParam::typed_scalar(
                Value::Int(value),
                ColumnType::Integer,
            )],
        )
        .unwrap();
        assert!(has_parameter(&selected.plan));
        entry.record_execution(selected.update);
    }
    let events = inputs.events.borrow();
    assert!(!events.contains(&"lookup"));
    assert!(!events.contains(&"publish"));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "optimize.generic")
            .count(),
        1
    );
    assert_eq!(
        events.iter().filter(|event| **event == "analysis").count(),
        1
    );
    assert_eq!(entry.generic_plans, 32);
    assert_eq!(inputs.entry.borrow().as_ref().unwrap().generic_plans, 0);
}

#[test]
fn analyzed_routine_variants_do_not_repeat_input_or_result_analysis() {
    for mode in ["force_generic_plan", "force_custom_plan"] {
        let inputs = Inputs {
            mode,
            ..Inputs::default()
        };
        inputs.fail_binding.set(true);
        let mut entry = inputs.entry.borrow().as_ref().unwrap().clone();
        for value in 1..=32 {
            let selected = select_analyzed_entry(
                &inputs.context(),
                &entry,
                &[SQLParam::typed_scalar(
                    Value::Int(value),
                    ColumnType::Integer,
                )],
            )
            .unwrap();
            entry.record_execution(selected.update);
        }
        let events = inputs.events.borrow();
        assert!(!events.contains(&"analysis"));
        assert!(!events.contains(&"binding"));
        assert!(!events.contains(&"lookup"));
        assert!(!events.contains(&"publish"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("optimize."))
                .count(),
            if mode == "force_generic_plan" { 1 } else { 32 }
        );
    }
}

#[test]
fn first_generic_cost_is_rechecked_before_selecting_the_executable_plan() {
    let inputs = Inputs::default();
    let selected = inputs.select().unwrap().unwrap();
    assert!(!has_parameter(&selected));
    assert_eq!(
        *inputs.events.borrow(),
        [
            "lookup",
            "mode",
            "analysis",
            "binding",
            "optimize.generic",
            "cost.generic",
            "optimize.custom",
            "cost.custom",
            "publish"
        ]
    );
    let entry = inputs.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert!(has_parameter(entry.plan.as_ref().unwrap()));
    assert_eq!(entry.generic_cost, Some(10_000.0));
    assert_eq!(entry.custom_plans, 6);
    assert_eq!(entry.generic_plans, 0);
    assert!(entry.total_custom_cost > 6.0);
}

#[test]
fn missing_definition_does_not_read_mode_or_capture_planning_inputs() {
    let inputs = Inputs {
        entry: RefCell::new(None),
        ..Inputs::default()
    };
    assert!(inputs.select().unwrap().is_none());
    assert_eq!(*inputs.events.borrow(), ["lookup"]);
}

#[test]
fn changed_result_descriptor_stops_before_optimization_and_publication() {
    let inputs = Inputs::default();
    inputs.entry.borrow_mut().as_mut().unwrap().result_schema = Some(RowSchema::with_types(
        vec!["changed".into()],
        vec![Some(ColumnType::Integer)],
    ));
    assert!(
        matches!(inputs.select(), Err(SQLError::Routine { sqlstate, message }) if sqlstate == "0A000" && message == "cached plan must not change result type")
    );
    assert_eq!(
        *inputs.events.borrow(),
        ["lookup", "mode", "analysis", "binding"]
    );
    let entry = inputs.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert!(entry.plan.is_none());
    assert_eq!(entry.custom_plans, 5);
}

#[test]
fn cached_generic_plan_reuses_its_descriptor_and_only_publishes_usage() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    {
        let mut entry = inputs.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        entry.plan = Some((*entry.logical_plan).clone());
        entry.generic_cost = Some(1.0);
    }
    assert!(has_parameter(&inputs.select().unwrap().unwrap()));
    assert_eq!(*inputs.events.borrow(), ["lookup", "mode", "publish"]);
    assert_eq!(inputs.entry.borrow().as_ref().unwrap().generic_plans, 1);
}

#[test]
fn invalidation_reads_original_inputs_and_only_publishes_successful_analysis() {
    let inputs = Inputs::default();
    let source = UnifiedPlan::lower(
        uqa_sql::compile("SELECT 'now'::timestamp AS value")
            .unwrap()
            .remove(0),
    );
    let _preparation_clock = uqa_sql::expr::TransactionClockScope::enter(90_123_456_789);
    let definition = uqa_sql::prepared::definition::analyze_definition(
        &inputs.context().analysis,
        source.clone(),
        &[],
    )
    .unwrap();
    {
        let mut entry = inputs.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        entry.source_plan = Arc::new(source);
        entry.logical_plan = Arc::new(definition.logical_plan);
        entry.effective_search_path = definition.effective_search_path;
        entry.parameter_types = definition.parameter_types;
        entry.result_schema = definition.result_schema;
    }
    for (invalidate, clock, expected) in [
        (false, 190_123_456_789, 90_123_456_789),
        (true, 290_123_456_789, 290_123_456_789),
        (false, 390_123_456_789, 290_123_456_789),
    ] {
        if invalidate {
            inputs.entry.borrow_mut().as_mut().unwrap().invalidate();
        }
        let _execution_clock = uqa_sql::expr::TransactionClockScope::enter(clock);
        let mut selected = select_plan(&inputs.context(), "saved", &[])
            .unwrap()
            .unwrap();
        let mut values = Vec::new();
        selected.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Temporal(uqa_core::TemporalValue::Timestamp { micros }),
                ..
            } = expression
            {
                values.push(*micros);
            }
        });
        assert_eq!(values, [expected]);
        assert!(!inputs.entry.borrow().as_ref().unwrap().needs_analysis);
    }
    let identity = {
        let mut entry = inputs.entry.borrow_mut();
        let entry = entry.as_mut().unwrap();
        entry.invalidate();
        entry.result_schema = Some(RowSchema::with_types(
            vec!["changed".into()],
            vec![Some(ColumnType::Timestamp)],
        ));
        entry.logical_plan.clone()
    };
    let error = select_plan(&inputs.context(), "saved", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    let entry = inputs.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert!(Arc::ptr_eq(&entry.logical_plan, &identity));
    assert!(entry.needs_analysis);
    assert!(entry.plan.is_none());
    assert_eq!(entry.generic_plans, 3);
}

#[test]
fn changed_creation_namespace_reanalyzes_inputs_and_replaces_the_cached_plan() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    inputs.prepare_temporal_input();
    let original = inputs.entry.borrow().as_ref().unwrap().clone();
    {
        let _clock = uqa_sql::expr::TransactionClockScope::enter(190_123_456_789);
        assert_eq!(
            temporal_inputs(&inputs.select().unwrap().unwrap()),
            [90_123_456_789]
        );
    }
    assert_eq!(
        *inputs.events.borrow(),
        ["lookup", "mode", "analysis", "binding", "publish"]
    );
    assert!(Arc::ptr_eq(
        &inputs.entry.borrow().as_ref().unwrap().logical_plan,
        &original.logical_plan,
    ));
    *inputs.search_path.borrow_mut() = vec!["pg_catalog".into(), "public".into()];
    inputs.events.borrow_mut().clear();
    {
        let _clock = uqa_sql::expr::TransactionClockScope::enter(290_123_456_789);
        assert_eq!(
            temporal_inputs(&inputs.select().unwrap().unwrap()),
            [290_123_456_789]
        );
    }
    assert_eq!(
        *inputs.events.borrow(),
        [
            "lookup",
            "mode",
            "analysis",
            "binding",
            "analysis",
            "binding",
            "analysis",
            "binding",
            "optimize.generic",
            "cost.generic",
            "publish"
        ]
    );
    let updated = inputs.entry.borrow().as_ref().unwrap().clone();
    assert!(!Arc::ptr_eq(&updated.logical_plan, &original.logical_plan));
    assert!(Arc::ptr_eq(&updated.source_plan, &original.source_plan));
    assert_eq!(
        temporal_inputs(updated.plan.as_ref().unwrap()),
        [290_123_456_789]
    );
    let current_path = updated.effective_search_path.as_ref().unwrap();
    assert_eq!(
        current_path.schemas,
        original.effective_search_path.as_ref().unwrap().schemas
    );
    assert_eq!(
        current_path.creation_namespace.as_deref(),
        Some("pg_catalog")
    );
    assert!(!updated.needs_analysis);
    assert_eq!(updated.parameter_types, original.parameter_types);
    assert_eq!(updated.source_sql, original.source_sql);
    assert_eq!(updated.prepared_at_micros, original.prepared_at_micros);
    assert_eq!(updated.from_sql, original.from_sql);
    assert_eq!(updated.custom_plans, original.custom_plans);
    assert_eq!(updated.total_custom_cost, original.total_custom_cost);
    assert_eq!(updated.generic_plans, 2);
    assert_eq!(updated.generic_cost, Some(10_000.0));

    // A path changed and restored before selection leaves the analyzed environment intact.
    *inputs.search_path.borrow_mut() = vec!["public".into()];
    *inputs.search_path.borrow_mut() = vec!["pg_catalog".into(), "public".into()];
    inputs.events.borrow_mut().clear();
    let _clock = uqa_sql::expr::TransactionClockScope::enter(390_123_456_789);
    assert_eq!(
        temporal_inputs(&inputs.select().unwrap().unwrap()),
        [290_123_456_789]
    );
    assert!(Arc::ptr_eq(
        &inputs.entry.borrow().as_ref().unwrap().logical_plan,
        &updated.logical_plan,
    ));
    assert_eq!(
        *inputs.events.borrow(),
        ["lookup", "mode", "analysis", "binding", "publish"]
    );
}

#[test]
fn changed_namespace_descriptor_failure_does_not_publish_the_new_analysis() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    inputs.prepare_temporal_input();
    let original = inputs.entry.borrow().as_ref().unwrap().clone();
    *inputs.search_path.borrow_mut() = vec!["pg_catalog".into(), "public".into()];
    inputs.entry.borrow_mut().as_mut().unwrap().result_schema = Some(RowSchema::with_types(
        vec!["changed".into(), "parameter".into()],
        vec![Some(ColumnType::Timestamp), Some(ColumnType::Integer)],
    ));
    let _clock = uqa_sql::expr::TransactionClockScope::enter(290_123_456_789);
    assert_eq!(inputs.select().unwrap_err().sqlstate(), Some("0A000"));
    {
        let entry = inputs.entry.borrow();
        let entry = entry.as_ref().unwrap();
        assert!(Arc::ptr_eq(&entry.logical_plan, &original.logical_plan));
        assert_eq!(entry.effective_search_path, original.effective_search_path);
        assert_eq!(
            temporal_inputs(entry.plan.as_ref().unwrap()),
            [90_123_456_789]
        );
        assert_eq!(entry.generic_cost, original.generic_cost);
        assert_eq!(entry.generic_plans, original.generic_plans);
        assert_eq!(entry.custom_plans, original.custom_plans);
        assert_eq!(entry.total_custom_cost, original.total_custom_cost);
        assert!(!entry.needs_analysis);
    }
    assert!(!inputs
        .events
        .borrow()
        .iter()
        .any(|event| event.starts_with("optimize") || *event == "publish"));

    inputs.entry.borrow_mut().as_mut().unwrap().result_schema = original.result_schema;
    assert_eq!(
        temporal_inputs(&inputs.select().unwrap().unwrap()),
        [290_123_456_789]
    );
    assert_eq!(inputs.entry.borrow().as_ref().unwrap().generic_plans, 1);
}

#[test]
fn namespace_read_failure_does_not_reuse_or_publish_the_cached_plan() {
    let inputs = Inputs {
        mode: "force_generic_plan",
        ..Inputs::default()
    };
    inputs.prepare_temporal_input();
    let original = inputs.entry.borrow().as_ref().unwrap().clone();
    inputs.fail_binding.set(true);
    assert!(
        matches!(inputs.select(), Err(SQLError::Internal(message)) if message == "namespace unavailable")
    );
    assert_eq!(
        *inputs.events.borrow(),
        ["lookup", "mode", "analysis", "binding"]
    );
    let entry = inputs.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert!(Arc::ptr_eq(&entry.logical_plan, &original.logical_plan));
    assert_eq!(entry.effective_search_path, original.effective_search_path);
    assert_eq!(
        temporal_inputs(entry.plan.as_ref().unwrap()),
        [90_123_456_789]
    );
    assert_eq!(entry.generic_cost, original.generic_cost);
    assert_eq!(entry.generic_plans, original.generic_plans);
    assert_eq!(entry.custom_plans, original.custom_plans);
    assert_eq!(entry.total_custom_cost, original.total_custom_cost);
}
