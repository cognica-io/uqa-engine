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
