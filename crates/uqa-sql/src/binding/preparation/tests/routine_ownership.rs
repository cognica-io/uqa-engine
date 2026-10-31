//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine parameter replacement preserves CALL syntax and observable query/RETURNING labels.

use super::*;
use crate::binding::RoutineParameterScope;
use crate::plan::{CommandPlan, CtePlanBody, MutableExecutablePlan, PlanRef};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn bind(sql: &str, control: &ProductionControl<'_>) -> Result<MutableExecutablePlan, SQLError> {
    let input = UnifiedPlan::lower(crate::compile(sql)?.remove(0));
    let params = [crate::SQLParam::typed_scalar(
        Value::Null,
        ColumnType::Integer,
    )];
    let parameters = RoutineParameterScope::new_with_control(
        "f",
        [("n", Some(&ColumnType::Integer))].into_iter(),
        control,
    )?;
    bind_routine_parameter_references_owned(
        &NoRoutines,
        input.copy_for_rewrite(control)?,
        &params,
        assignment_context().copy_with_control(control)?,
        &parameters,
        control,
    )
}

#[test]
fn owned_routine_references_bind_named_call_values_without_selecting_the_procedure() {
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for sql in [
        "CALL absent(n)",
        "CALL absent(y => n, x => 1)",
        "CALL absent(y => f.n, x => n + 1)",
        "CALL absent(n => n)",
        "CALL absent(unknown_function(n))",
    ] {
        let mut expected = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let parameters =
            RoutineParameterScope::new("f", vec!["n".into()], vec![Some(ColumnType::Integer)]);
        crate::binding::bind_routine_parameter_references(
            &NoRoutines,
            &mut expected,
            &[crate::SQLParam::typed_scalar(
                Value::Null,
                ColumnType::Integer,
            )],
            &assignment_context(),
            &parameters,
        )
        .unwrap();
        let actual = bind(sql, &control).unwrap();
        let PlanRef::Command(actual_command) = actual.as_plan_ref() else {
            panic!("CALL")
        };
        let UnifiedPlan::Command(expected_command) = &expected else {
            panic!("CALL")
        };
        assert_eq!(
            serde_json::to_value(actual_command).unwrap(),
            serde_json::to_value(expected_command).unwrap(),
            "{sql}"
        );
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

fn command_returning(plan: PlanRef<'_>) -> &[crate::plan::ProjectionPlan] {
    let command = match plan {
        PlanRef::Command(command) => command,
        PlanRef::Query(query) => match &query.ctes[0].body {
            CtePlanBody::Command(command) => command,
            CtePlanBody::Query(_) => panic!("mutation CTE"),
        },
    };
    match command {
        CommandPlan::Insert(plan) => &plan.returning,
        CommandPlan::Update(plan) => &plan.returning,
        CommandPlan::Delete(plan) => &plan.returning,
        CommandPlan::Merge(plan) => &plan.returning,
        _ => panic!("mutation"),
    }
}

#[test]
fn owned_routine_references_preserve_returning_labels_in_all_mutations_and_ctes() {
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for mutation in [
        "INSERT INTO assignment_target(id) VALUES(n)",
        "UPDATE assignment_target SET id=n",
        "DELETE FROM assignment_target WHERE id=n",
        "MERGE INTO assignment_target USING (VALUES(1)) s(k) ON assignment_target.id=s.k WHEN MATCHED THEN UPDATE SET id=n",
    ] {
        for cte in [false, true] {
            let sql = format!("{mutation} RETURNING n, f.n AS explicit, id");
            let sql = if cte { format!("WITH x AS ({sql}) SELECT x.n FROM x") } else { sql };
            let actual = bind(&sql, &control).unwrap_or_else(|error| panic!("{sql}: {error}"));
            let projections = command_returning(actual.as_plan_ref());
            assert!(matches!(projections[0].expr, crate::ScalarExpr::Param(1)), "{sql}");
            assert_eq!(projections[0].alias.as_deref(), Some("n"), "{sql}");
            assert_eq!(projections[1].alias.as_deref(), Some("explicit"), "{sql}");
            assert!(projections[2].alias.is_none(), "{sql}");
            assert!(budget.used() > 0);
            drop(actual);
            assert_eq!(budget.used(), 0, "{sql}");
        }
    }
}

#[test]
fn owned_routine_reference_failures_retire_private_parameter_and_alias_storage() {
    let sql =
        "WITH x AS (INSERT INTO assignment_target(id) VALUES(n) RETURNING n) SELECT x.n FROM x";
    for failure in 0..3 {
        let budget = MemoryBudget::new(if failure == 0 { 4096 } else { 4 << 20 });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let error = bind(sql, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
    }
}
