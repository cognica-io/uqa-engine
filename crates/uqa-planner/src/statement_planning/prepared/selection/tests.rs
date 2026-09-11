//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
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
                logical_plan,
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
}
fn has_parameter(plan: &UnifiedPlan) -> bool {
    let mut found = false;
    plan.clone().rewrite_scalar_expressions(&mut |expression| {
        found |= matches!(expression, ScalarExpr::Param(_));
    });
    found
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
        Ok(fixtures::binding_context())
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
