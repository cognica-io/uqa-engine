//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_sql::{
    ast::{ColumnType, FunctionBinding},
    binding::statements::{StatementAnalysisOperation, StatementAnalysisScopes},
    routines::RoutineResolution,
    FunctionTypeResolver,
};

struct Inputs {
    captured: Cell<bool>,
}
impl StatementAnalysisScopes for Inputs {
    fn with_scope(&self, _: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        self.captured.set(true);
        Err(SQLError::Internal("analysis scope unavailable".into()))
    }
}
impl StatementOptimizationContexts for Inputs {
    fn statistics(&self) -> StatementStatisticsContext<'_> {
        panic!("analysis failed before optimizer input capture")
    }
    fn rule_inputs(&self) -> RuleInputPlanningContext<'_> {
        panic!("analysis failed before optimizer rule capture")
    }
}
struct NoRoutines;
impl FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for NoRoutines {}
impl uqa_sql::schema::dependencies::oid_alias::OidAliasInput for NoRoutines {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        panic!("analysis scope acquisition failed")
    }
}

#[test]
fn failed_sql_analysis_precedes_optimizer_inputs_and_constant_evaluation() {
    let inputs = Inputs {
        captured: Cell::new(false),
    };
    let aggregates = |_: &str| false;
    let context = StatementPlanningContext {
        analysis: StatementAnalysisContext {
            scopes: &inputs,
            routines: &NoRoutines,
            aliases: &NoRoutines,
        },
        aggregates: &aggregates,
        optimization: &inputs,
        constant_evaluator: uqa_execution::scalar::eval_constant_scalar,
    };
    let plan = UnifiedPlan::lower(uqa_sql::compile("SELECT 1 / 0").unwrap().remove(0));
    let error = plan_for_execution(&context, plan, &[]).unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "analysis scope unavailable")
    );
    assert!(inputs.captured.get());
}

#[test]
fn analyzed_rule_conditions_discard_dead_inputs_but_retain_reachable_planning_errors() {
    let inputs = Inputs {
        captured: Cell::new(false),
    };
    let context = StatementPlanningContext {
        analysis: StatementAnalysisContext {
            scopes: &inputs,
            routines: &NoRoutines,
            aliases: &NoRoutines,
        },
        aggregates: &|_: &str| false,
        optimization: &inputs,
        constant_evaluator: uqa_execution::scalar::eval_constant_scalar,
    };
    for (sql, fails) in [
        ("SELECT CASE WHEN true THEN false ELSE 1/0>0 END", false),
        (
            "SELECT CASE WHEN true THEN false ELSE (SELECT 1/0)>0 END",
            false,
        ),
        ("SELECT CASE WHEN $1 THEN false ELSE 1/0>0 END", true),
    ] {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query");
        };
        let uqa_sql::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("query block");
        };
        let mut plan = uqa_sql::plan::ExpressionPlan {
            scalar: block.projections.remove(0).expr,
            subqueries: block.subqueries,
        };
        let result = crate::statement_planning::schema_expressions::optimize_rule_condition(
            &context, &mut plan,
        );
        if fails {
            assert_eq!(result.unwrap_err().sqlstate(), Some("22012"));
        } else {
            result.unwrap();
            assert_eq!(
                plan.scalar,
                uqa_sql::ScalarExpr::Literal(uqa_core::Value::Bool(false))
            );
            assert!(plan.subqueries.is_empty());
        }
    }
    assert!(!inputs.captured.get());
}
