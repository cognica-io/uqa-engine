//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::statement::context::StatementExecutionContext;
use std::cell::RefCell;
use uqa_core::Value;
use uqa_sql::plan::CtePlanBody;

#[derive(Default)]
struct Inputs {
    classified: RefCell<Vec<String>>,
    planned: RefCell<Vec<UnifiedPlan>>,
}

impl Inputs {
    fn context(&self) -> CompiledStatementContext<'_, ()> {
        CompiledStatementContext {
            aggregates: self,
            planning: self,
            statements: self,
        }
    }
}

impl AggregateClassifier for Inputs {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self.classified.borrow_mut().push(name.to_owned());
        name == "registered_total"
    }
}

impl ExecutablePlanOptimizer for Inputs {
    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<UnifiedPlan, SQLError> {
        assert!(matches!(params, [SQLParam::Scalar(Value::Int(7))]));
        self.planned.borrow_mut().push(plan);
        Err(SQLError::TypeMismatch("planning rejected the input".into()))
    }
}

impl StatementExecutionInputs<()> for Inputs {
    fn statement_execution_context(&self) -> StatementExecutionContext<'_, ()> {
        panic!("failed analysis must not capture execution state or start execution")
    }
}

fn statement(sql: &str) -> Statement {
    let mut statements = uqa_sql::compile(sql).unwrap();
    assert_eq!(statements.len(), 1);
    statements.remove(0)
}

fn assert_planning_error(result: Result<SQLResult, SQLError>) {
    assert!(
        matches!(result, Err(SQLError::TypeMismatch(message)) if message == "planning rejected the input")
    );
}

#[test]
fn registered_aggregate_lowering_and_parameters_precede_execution_capture() {
    let inputs = Inputs::default();
    assert_planning_error(execute(
        &inputs.context(),
        statement("SELECT registered_total($1) FROM public.items"),
        &[SQLParam::scalar(Value::Int(7))],
    ));
    assert!(inputs
        .classified
        .borrow()
        .iter()
        .any(|name| name == "registered_total"));
    let plans = inputs.planned.borrow();
    assert!(matches!(&plans[..], [UnifiedPlan::Query(query)] if !query.relations_bound));
}

#[test]
fn catalog_relation_identity_is_retained_before_planning_nested_queries() {
    let inputs = Inputs::default();
    assert_planning_error(execute_with_privilege_subject(
        &inputs.context(),
        statement("WITH source AS (SELECT $1 AS id FROM public.items) SELECT id FROM source"),
        &[SQLParam::scalar(Value::Int(7))],
        "rule_owner",
    ));
    let plans = inputs.planned.borrow();
    let [UnifiedPlan::Query(query)] = &plans[..] else {
        panic!("expected the bound catalog query")
    };
    assert!(query.relations_bound);
    assert!(matches!(&query.ctes[0].body, CtePlanBody::Query(source) if source.relations_bound));
}

#[test]
fn invalid_catalog_commands_fail_before_planner_or_execution_inputs() {
    let inputs = Inputs::default();
    let result = execute_with_privilege_subject(
        &inputs.context(),
        statement("CREATE TABLE rejected (id BIGINT)"),
        &[SQLParam::scalar(Value::Int(7))],
        "rule_owner",
    );
    assert!(
        matches!(result, Err(SQLError::Internal(message)) if message == "catalog-owned statement lowered to an unsupported command")
    );
    assert!(inputs.planned.borrow().is_empty());
}

#[test]
fn existing_plan_keeps_bound_relations_without_reclassifying_aggregates() {
    let inputs = Inputs::default();
    let mut plan = UnifiedPlan::lower(statement("SELECT $1 FROM public.items"));
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("expected a query")
    };
    query.relations_bound = true;
    assert_planning_error(execute_plan(
        &inputs.context(),
        plan,
        &[SQLParam::scalar(Value::Int(7))],
    ));
    assert!(inputs.classified.borrow().is_empty());
    let plans = inputs.planned.borrow();
    assert!(matches!(&plans[..], [UnifiedPlan::Query(query)] if query.relations_bound));
}
