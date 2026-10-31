//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{CommandAssignmentSlot, MutableExecutablePlan, NoRegisteredAggregates};

fn edit_fields(
    mut input: MutableCommandPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    input = input.retarget_mutation(control.copy_text("public.retargeted_base")?, true, control)?;
    input = input.set_target_privilege_subject(
        RoleReference::Named("owner".into()).clone_with_control(control)?,
        control,
    )?;
    match &*input {
        CommandPlan::Insert(_) => {
            input = input
                .record_view_rule_relation("public.v", control)?
                .record_view_insert_inputs("public.v", control)?;
            input = input.map_insert_column(0, control, |_| Ok(control.copy_text("mapped")?))?;
            input = input.map_conflict_columns(control, |_| Ok(control.copy_text("mapped")?))?;
            input = input.map_assignment_columns(
                CommandAssignmentSlot::Conflict(0),
                control,
                |_| Ok(control.copy_text("mapped")?),
            )?;
            input = input.map_scalar_input(
                CommandScalarSlot::ConflictInferencePredicate,
                control,
                |value, _| Ok(value),
            )?;
            input = input.finalize_view_insert_inputs(control)?;
        }
        CommandPlan::Update(_) => {
            input = input
                .record_view_rule_relation("public.v", control)?
                .record_view_update_inputs("public.v", control)?;
            input =
                input.map_assignment_columns(CommandAssignmentSlot::Update(0), control, |_| {
                    Ok(control.copy_text("mapped")?)
                })?;
            input = input.finalize_view_update_inputs(control)?;
        }
        CommandPlan::Merge(_) => {
            input = input.map_assignment_columns(
                CommandAssignmentSlot::Merge {
                    clause: 0,
                    assignment: 0,
                },
                control,
                |_| Ok(control.copy_text("mapped")?),
            )?;
            input = input.set_implicit_merge_columns(1, ["a"].into_iter(), control)?;
            input = input
                .map_merge_insert_column(1, 0, control, |_| Ok(control.copy_text("mapped")?))?;
            input = input.map_predicate_input(control, |_, _| {
                Ok(Some(
                    ScalarExpr::Literal(Value::Bool(true)).copy_for_rewrite(control)?,
                ))
            })?;
        }
        _ => unreachable!(),
    }
    input.prepend_view_check(control.copy_text("public.v")?, control, |_| {
        Ok(ScalarExpr::Literal(Value::Bool(true)).copy_for_rewrite(control)?)
    })
}

#[test]
fn view_field_edits_preserve_exact_storage_from_both_lowered_and_copied_commands() {
    for sql in [
        "INSERT INTO items(a[1]) VALUES(10) ON CONFLICT(a) WHERE a>0 DO UPDATE SET a[2]=30 WHERE a>1 RETURNING a",
        "UPDATE items SET a[1]=2 WHERE a=1 RETURNING a",
        "MERGE INTO items t USING (VALUES(1)) s(a) ON t.a=s.a WHEN MATCHED THEN UPDATE SET a[1]=2 WHEN NOT MATCHED THEN INSERT VALUES(7) RETURNING t.a",
    ] {
        let syntax = crate::compile(sql).unwrap().remove(0);
        let original = command(sql);
        for lower in [false, true] {
            let budget = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let input = if lower {
                let MutableExecutablePlan::Command(input) = UnifiedPlan::lower_borrowed_for_rewrite(&syntax, &NoRegisteredAggregates, &control).unwrap() else { panic!("command") };
                input
            } else { copy(&original, &control).unwrap() };
            let query_address = arena(&input).as_ptr();
            let before = budget.used();
            let mut output = None;
            let measured = allocation_counter::measure(|| { output = Some(edit_fields(input, &control).unwrap()); });
            let output = output.unwrap();
            assert_eq!(arena(&output).as_ptr(), query_address);
            assert_eq!(before as i64 + measured.bytes_current, budget.used() as i64, "{sql}/{lower}: {measured:?}");
            assert_eq!(output.reserved_bytes(), budget.used());
            match &*output {
                CommandPlan::Insert(plan) => {
                    assert_eq!(plan.view_rule_insert_plans[0].supplied_columns, ["a"]);
                    assert_eq!(plan.view_rule_insert_plans[0].input_columns, ["mapped"]);
                    assert_eq!(plan.columns[0].indirection.len(), 1);
                    assert_eq!(plan.on_conflict.as_ref().unwrap().conflict_columns, ["mapped"]);
                    assert_eq!(plan.view_checks.len(), 1);
                }
                CommandPlan::Update(plan) => {
                    assert_eq!(plan.view_rule_update_plans[0].assigned_columns, ["a"]);
                    assert_eq!(plan.view_rule_update_plans[0].input_columns, ["mapped"]);
                    assert_eq!(plan.assignments[0].target.targets()[0].indirection.len(), 1);
                    assert_eq!(plan.view_checks.len(), 1);
                }
                CommandPlan::Merge(plan) => {
                    let crate::plan::MergeWhenPlan::InsertNotMatched { columns, .. } = &plan.when_clauses[1] else { panic!("INSERT action") };
                    assert_eq!(columns[0].column, "mapped");
                    assert!(matches!(plan.target_predicate, Some(ScalarExpr::Literal(Value::Bool(true)))));
                    assert_eq!(plan.view_checks.len(), 1);
                }
                _ => unreachable!(),
            }
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn partial_view_column_translation_releases_replacements_on_rejection_cancellation_and_unwind() {
    let source = command("INSERT INTO items(a,b) VALUES(1,2) ON CONFLICT(a,b) DO NOTHING");
    let large = "x".repeat(4 << 20);
    for failure in 0..4 {
        let budget = MemoryBudget::new(16 << 20);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        let sibling = control.copy_text("sibling").unwrap();
        let baseline = budget.used();
        let input = copy(&source, &control).unwrap();
        let occupied = budget.reserve(budget.available() - (128 << 10)).unwrap();
        let mut seen = 0;
        let measured = allocation_counter::measure(|| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                input.map_conflict_columns(&control, |_| {
                    seen += 1;
                    if seen == 1 {
                        return Ok(control.copy_text("translated")?);
                    }
                    match failure {
                        0 => Ok(control.copy_text(&large)?),
                        1 => {
                            statement.cancel();
                            Ok(control.copy_text("cancelled")?)
                        }
                        2 => {
                            session.cancel();
                            Ok(control.copy_text("cancelled")?)
                        }
                        _ => panic!("view column callback"),
                    }
                })
            }));
            if failure == 3 {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result.unwrap().unwrap_err().sqlstate(),
                    Some(if failure == 0 { "53200" } else { "57014" })
                );
            }
        });
        assert_eq!(seen, 2);
        assert!(measured.bytes_total < 32 << 10, "{measured:?}");
        assert_eq!(budget.used(), baseline + occupied.bytes());
        assert_eq!(&**sibling, "sibling");
        drop(occupied);
        let next = CancellationToken::new();
        let recovered = ProductionControl::new(&budget, &next, &next);
        drop(copy(&source, &recovered).unwrap());
        assert_eq!(budget.used(), baseline);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
