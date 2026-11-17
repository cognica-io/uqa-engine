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

#[test]
fn owned_routine_references_preserve_stored_query_scopes_and_diagnostics() {
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let parameters =
        RoutineParameterScope::new("f", vec!["n".into()], vec![Some(ColumnType::Integer)]);
    let params = [crate::SQLParam::typed_scalar(
        Value::Null,
        ColumnType::Integer,
    )];
    for sql in [
        "SELECT n, f.n, n + 1",
        "SELECT n, f.n FROM assignment_target",
        "SELECT n FROM (VALUES (1)) t(n)",
        "SELECT n, f.n FROM (VALUES (1)) f(n)",
        "SELECT n AS label ORDER BY label",
        "SELECT id AS n FROM assignment_target ORDER BY n",
        "SELECT n FROM assignment_target GROUP BY n",
        "SELECT id AS n FROM assignment_target GROUP BY n",
        "SELECT DISTINCT ON (n) id AS n FROM assignment_target ORDER BY n",
        "WITH c AS (SELECT n) SELECT n FROM c",
        "WITH c AS (SELECT n) SELECT f.n FROM c",
        "SELECT (SELECT n) FROM assignment_target",
        "SELECT (SELECT n FROM (VALUES (1)) t(n))",
        "SELECT * FROM (SELECT n) t",
        "SELECT * FROM generate_series(1, n)",
        "SELECT sum(id) OVER (PARTITION BY n) FROM assignment_target",
        "SELECT sum(id) OVER (ORDER BY id ROWS BETWEEN n PRECEDING AND CURRENT ROW) FROM assignment_target",
        "SELECT sum(id) OVER w FROM assignment_target WINDOW w AS (PARTITION BY n ORDER BY id)",
        "SELECT n UNION ALL SELECT n",
        "VALUES (n), (f.n)",
        "SELECT absent(n)",
        "SELECT missing FROM assignment_target",
        "SELECT n FROM absent",
    ] {
        let mut expected = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut ordinary = expected.clone();
        // Composite/variable consumers retain this scoped walk. Its existing parameter observation is independent of the shared preparation entry points.
        let mut scope = SchemaScope::for_analysis(&assignment_context()).unwrap();
        scope.routine_parameters = Some(parameters.clone());
        scope.scalar_binding = crate::binding::ScalarBindingMode::References;
        scope.preserve_syntax_shape = true;
        let expected_result = scope.bind_statement_parameters(
            &NoRoutines,
            &mut expected,
            &params,
            Some(parameters.schema()),
        );
        let ordinary_result = crate::binding::bind_routine_parameter_references(
            &NoRoutines,
            &mut ordinary,
            &params,
            &assignment_context(),
            &parameters,
        );
        let actual = bind(sql, &control);
        match (actual, expected_result) {
            (Ok(actual), Ok(())) => {
                ordinary_result.unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(
                    serde_json::to_value(&ordinary).unwrap(),
                    serde_json::to_value(&expected).unwrap(),
                    "ordinary {sql}"
                );
                let actual = match actual.as_plan_ref() {
                    PlanRef::Query(query) => serde_json::to_value(query).unwrap(),
                    PlanRef::Command(command) => serde_json::to_value(command).unwrap(),
                };
                let expected = match &expected {
                    UnifiedPlan::Query(query) => serde_json::to_value(query).unwrap(),
                    UnifiedPlan::Command(command) => serde_json::to_value(command).unwrap(),
                };
                assert_eq!(actual, expected, "{sql}");
            }
            (Err(actual), Err(expected)) => {
                assert_eq!(actual.sqlstate(), expected.sqlstate(), "{sql}");
                assert_eq!(actual.to_string(), expected.to_string(), "{sql}");
                let ordinary = ordinary_result.unwrap_err();
                assert_eq!(ordinary.sqlstate(), expected.sqlstate(), "ordinary {sql}");
                assert_eq!(ordinary.to_string(), expected.to_string(), "ordinary {sql}");
            }
            (actual, expected) => panic!("{sql}: actual={actual:?}; expected={expected:?}"),
        }
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn prepared_input_owners_preserve_original_sites_across_task_transfer() {
    for sql in [
        "SELECT '7'::smallint, '7'::bigint, $1 + 2",
        "WITH c(x) AS (SELECT $1::integer) SELECT x, '7'::smallint FROM c ORDER BY x",
        "SELECT x, count(*) FROM (VALUES (1), (2)) v(x) GROUP BY GROUPING SETS ((x), ())",
        "SELECT x, ('2'::smallint = ANY (ARRAY['2'::integer, '3'::integer])) FROM (SELECT NULL AS x UNION ALL SELECT 2::smallint) s",
        "SELECT sum(x) OVER (PARTITION BY $1 ORDER BY x ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM (VALUES (1), (2)) v(x)",
    ] {
        let mut expected = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let input = expected.clone();
        let binding = assignment_context();
        let declared = [Some(ColumnType::Integer)];
        let reference = read_prepared_inputs(&NoRoutines, &mut expected, &declared, &binding, None).unwrap();
        let budget = MemoryBudget::new(16 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut actual = input.copy_for_rewrite(&control).unwrap();
        let mut scope = SchemaScope::for_analysis_with_control(binding.copy_with_control(&control).unwrap(), &control).unwrap();
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope: &mut scope,
            parameters: ParameterTypes::with_input_constants_with_control(&declared, None, None, None, &control).unwrap().retain_result_metadata(),
            schema_expression: None,
        };
        let PlanRef::Query(query) = actual.as_plan_ref() else { panic!("query") };
        analysis.query(query, None).unwrap();
        let constants = analysis.parameters.take_input_constants();
        let parameters = analysis.parameters.finish_produced().unwrap();
        assert_eq!(&*parameters, &reference.parameter_types, "{sql}");
        let params = crate::prepared::analysis_parameters_with_control(&parameters, &control).unwrap();
        std::thread::scope(|tasks| {
            tasks.spawn(|| constants.apply_owned(&mut actual, &NoRoutines, &params, &control)).join().unwrap()
        }).unwrap();
        let actual = retain_prepared_input_metadata_owned(&NoRoutines, actual, &parameters, binding.copy_with_control(&control).unwrap(), &control).unwrap();
        let PlanRef::Query(query) = actual.as_plan_ref() else { panic!("query") };
        let UnifiedPlan::Query(expected) = expected else { panic!("query") };
        assert_eq!(serde_json::to_value(query).unwrap(), serde_json::to_value(expected).unwrap(), "{sql}");
        drop(actual);
        drop(parameters);
        drop(params);
        drop(scope);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn routine_analysis_state_transfers_with_its_original_scopes_and_observations() {
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let params = [crate::SQLParam::typed_scalar(
        Value::Null,
        ColumnType::Integer,
    )];
    let parameters =
        RoutineParameterScope::new("f", vec!["n".into()], vec![Some(ColumnType::Integer)]);
    for sql in [
        "SELECT n, f.n + 1",
        "WITH x AS (SELECT n AS v) SELECT x.v, f.n FROM x",
        "SELECT n, (SELECT f.n + t.id) FROM assignment_target t",
        "SELECT n FROM (VALUES (1), (2)) t(n) WHERE n = f.n",
        "SELECT sum(x) OVER (PARTITION BY n ORDER BY x ROWS BETWEEN n PRECEDING AND CURRENT ROW) FROM (VALUES (1), (2)) v(x)",
        "INSERT INTO assignment_target(id) VALUES(n) RETURNING n, id",
        "UPDATE assignment_target SET id=n RETURNING f.n, id",
        "DELETE FROM assignment_target WHERE id=n RETURNING f.n, id",
    ] {
        let mut expected = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut actual = expected.copy_for_rewrite(&control).unwrap();
        crate::binding::bind_routine_parameter_references(&NoRoutines, &mut expected, &params, &assignment_context(), &parameters).unwrap();
        let mut scope = SchemaScope::for_analysis_with_control(assignment_context().copy_with_control(&control).unwrap(), &control).unwrap();
        scope.routine_parameters = Some(parameters.clone());
        scope.scalar_binding = crate::binding::ScalarBindingMode::References;
        scope.preserve_syntax_shape = true;
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope: &mut scope,
            parameters: ParameterTypes::fixed_with_control(&params, &control).unwrap().with_reference_retention(),
            schema_expression: (),
        };
        let plan = actual.as_plan_ref();
        let parameters = &parameters;
        let mut analysis = std::thread::scope(|tasks| {
            tasks.spawn(move || {
                analysis.observe_routine_parameter_references(plan, parameters.schema()).unwrap();
                analysis
            }).join().unwrap()
        });
        let references = analysis.parameters.take_reference_constants();
        drop(analysis);
        references.apply_owned(&mut actual, &NoRoutines, &params, &control).unwrap();
        let actual = super::super::production::normalize_windows(actual, &control).unwrap();
        let observed = match actual.as_plan_ref() {
            PlanRef::Query(query) => serde_json::to_value(query).unwrap(),
            PlanRef::Command(command) => serde_json::to_value(command).unwrap(),
        };
        let expected = match expected {
            UnifiedPlan::Query(query) => serde_json::to_value(query).unwrap(),
            UnifiedPlan::Command(command) => serde_json::to_value(command).unwrap(),
        };
        assert_eq!(observed, expected, "{sql}");
        drop(actual);
        drop(scope);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn transferred_routine_analysis_retires_partial_state_without_changing_its_input() {
    for failure in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = UnifiedPlan::lower(crate::compile("SELECT n, f.n + 1").unwrap().remove(0));
        let plan = input.copy_for_rewrite(&control).unwrap();
        let independent = budget.reserve(256).unwrap();
        let retained = budget.used();
        let params = [crate::SQLParam::typed_scalar(
            Value::Null,
            ColumnType::Integer,
        )];
        let parameters =
            RoutineParameterScope::new("f", vec!["n".into()], vec![Some(ColumnType::Integer)]);
        let mut scope = SchemaScope::for_analysis_with_control(
            assignment_context().copy_with_control(&control).unwrap(),
            &control,
        )
        .unwrap();
        scope.routine_parameters = Some(parameters.clone());
        scope.scalar_binding = crate::binding::ScalarBindingMode::References;
        scope.preserve_syntax_shape = true;
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope: &mut scope,
            parameters: ParameterTypes::fixed_with_control(&params, &control)
                .unwrap()
                .with_reference_retention(),
            schema_expression: (),
        };
        let PlanRef::Query(query) = plan.as_plan_ref() else {
            panic!("query")
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        analysis
            .expression(&block.projections[0].expr, parameters.schema(), &[])
            .unwrap();
        assert!(budget.used() > retained);
        let occupied = (failure == 3).then(|| budget.reserve(budget.available()).unwrap());
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        std::thread::scope(|tasks| {
            tasks
                .spawn(move || {
                    if failure != 0 {
                        let error = analysis
                            .expression(&block.projections[1].expr, parameters.schema(), &[])
                            .err()
                            .unwrap();
                        assert_eq!(
                            error.sqlstate(),
                            Some(if failure == 3 { "53200" } else { "57014" })
                        );
                    }
                    drop(analysis);
                })
                .join()
                .unwrap();
        });
        drop(scope);
        drop(occupied);
        assert_eq!(budget.used(), retained);
        let UnifiedPlan::Query(expected) = &input else {
            panic!("query")
        };
        assert_eq!(
            serde_json::to_value(query).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        drop(plan);
        assert_eq!(budget.used(), 256);
        drop(independent);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scheduled_routine_analysis_preserves_stored_and_source_set_placement() {
    use crate::{
        catalog::work::inline,
        construction::{receive, Driver},
        semantics::sets::placement_scope::{placing_set_calls, SetPlacementScope},
    };
    for sql in [
        "SELECT CASE WHEN true THEN generate_series(1, 2) END",
        "SELECT coalesce(generate_series(1, 2), 0)",
        "SELECT sum(generate_series(1, 2))",
    ] {
        for stored in [false, true] {
            let budget = MemoryBudget::new(4 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
            let expected = {
                let _scope = if stored {
                    SetPlacementScope::stored_definition()
                } else {
                    SetPlacementScope::compiled_source()
                };
                crate::binding::infer_prepared_parameter_types(
                    &NoRoutines,
                    &plan,
                    &[],
                    &assignment_context(),
                )
            };
            assert_eq!(expected.is_ok(), stored, "{sql}");
            let UnifiedPlan::Query(query) = &plan else {
                panic!("query")
            };
            let mut scope = SchemaScope::for_analysis_with_control(
                assignment_context().copy_with_control(&control).unwrap(),
                &control,
            )
            .unwrap();
            let mut analysis = Preparation {
                routines: &NoRoutines,
                scope: &mut scope,
                parameters: ParameterTypes::fixed_with_control(&[], &control)
                    .unwrap()
                    .with_reference_retention(),
                schema_expression: (),
                bound_condition_queries: None,
            };
            let mut driver = Driver::new(control).unwrap();
            let output = {
                let _scope = if stored {
                    SetPlacementScope::stored_definition()
                } else {
                    SetPlacementScope::compiled_source()
                };
                driver
                    .work()
                    .submit(move |mut work| async move {
                        work.checkpoint().await;
                        let result = analysis.query(query, None);
                        work.checkpoint().await;
                        result
                    })
                    .unwrap()
            };
            // The executor's unrelated mode must neither change this analysis nor receive its mode.
            let invoking_scope = if stored {
                SetPlacementScope::compiled_source()
            } else {
                SetPlacementScope::stored_definition()
            };
            while inline(driver.advance()).unwrap() {
                assert_eq!(placing_set_calls(), stored);
            }
            let actual = inline(receive(output));
            match (actual, expected) {
                (Ok(_), Ok(_)) => (),
                (Err(actual), Err(expected)) => {
                    assert_eq!(actual.sqlstate(), Some("0A000"), "{sql}");
                    assert_eq!(actual.sqlstate(), expected.sqlstate(), "{sql}");
                    assert_eq!(actual.to_string(), expected.to_string(), "{sql}");
                    assert_eq!(actual.hint(), expected.hint(), "{sql}");
                }
                (actual, expected) => panic!("{sql}: {actual:?}; {expected:?}"),
            }
            drop(driver);
            drop(scope);
            assert_eq!(placing_set_calls(), stored);
            drop(invoking_scope);
            assert!(placing_set_calls());
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn transferred_input_failures_retire_observations_and_preserve_original_owners() {
    for failure in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = UnifiedPlan::lower(crate::compile("SELECT n, f.n").unwrap().remove(0));
        let mut plan = input.copy_for_rewrite(&control).unwrap();
        let independent = budget.reserve(256).unwrap();
        let retained = budget.used();
        let parameters =
            RoutineParameterScope::new("f", vec!["n".into()], vec![Some(ColumnType::Integer)]);
        let params = [crate::SQLParam::typed_scalar(
            Value::Null,
            ColumnType::Integer,
        )];
        let mut scope = SchemaScope::for_analysis_with_control(
            assignment_context().copy_with_control(&control).unwrap(),
            &control,
        )
        .unwrap();
        scope.routine_parameters = Some(parameters.clone());
        scope.scalar_binding = crate::binding::ScalarBindingMode::References;
        scope.preserve_syntax_shape = true;
        let mut analysis = Preparation {
            bound_condition_queries: None,
            routines: &NoRoutines,
            scope: &mut scope,
            parameters: ParameterTypes::fixed_with_control(&params, &control)
                .unwrap()
                .with_reference_retention(),
            schema_expression: (),
        };
        let PlanRef::Query(query) = plan.as_plan_ref() else {
            panic!("query")
        };
        analysis.query(query, Some(parameters.schema())).unwrap();
        let constants = analysis.parameters.take_reference_constants();
        drop(analysis);
        drop(scope);
        assert!(budget.used() > retained);
        let occupied = (failure == 3).then(|| budget.reserve(budget.available()).unwrap());
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        std::thread::scope(|tasks| {
            tasks
                .spawn(|| {
                    if failure == 0 {
                        drop(constants);
                    } else {
                        let error = constants
                            .apply_owned(&mut plan, &NoRoutines, &params, &control)
                            .unwrap_err();
                        assert_eq!(
                            error.sqlstate(),
                            Some(if failure == 3 { "53200" } else { "57014" })
                        );
                    }
                })
                .join()
                .unwrap();
        });
        let PlanRef::Query(actual) = plan.as_plan_ref() else {
            panic!("query")
        };
        let UnifiedPlan::Query(expected) = &input else {
            panic!("query")
        };
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(
            budget.used(),
            retained
                + occupied
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        drop(plan);
        assert_eq!(budget.used(), independent.bytes());
        drop(independent);
        assert_eq!(budget.used(), 0);
    }
}
