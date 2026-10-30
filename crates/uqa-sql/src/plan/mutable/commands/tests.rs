//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::roles::RoleReference,
    plan::{RelationalPlan, UnifiedPlan},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn command(sql: &str) -> CommandPlan {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    *command
}
fn commands() -> [CommandPlan; 4] {
    [
        command("WITH q AS (SELECT 7) INSERT INTO items VALUES (1) RETURNING id"),
        command("WITH q AS (SELECT 7) UPDATE items SET id = 2 WHERE id = 1 RETURNING id"),
        command("WITH q AS (SELECT 7) DELETE FROM items WHERE id = 1 RETURNING id"),
        command("WITH q AS (SELECT 7) MERGE INTO items t USING (VALUES (1)) s(id) ON t.id = s.id WHEN MATCHED THEN UPDATE SET id = s.id RETURNING t.id"),
    ]
}
fn copy(
    input: &CommandPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    match input {
        CommandPlan::Insert(plan) => plan.copy_for_rewrite(control),
        CommandPlan::Update(plan) => plan.copy_for_rewrite(control),
        CommandPlan::Delete(plan) => plan.copy_for_rewrite(control),
        CommandPlan::Merge(plan) => plan.copy_for_rewrite(control),
        _ => unreachable!(),
    }
}
fn arena(input: &CommandPlan) -> &[crate::plan::QueryPlan] {
    match input {
        CommandPlan::Insert(plan) => &plan.subqueries,
        CommandPlan::Update(plan) => &plan.subqueries,
        CommandPlan::Delete(plan) => &plan.subqueries,
        CommandPlan::Merge(plan) => &plan.subqueries,
        _ => unreachable!(),
    }
}
fn add_payload(input: &mut CommandPlan, bytes: usize) {
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(bytes)));
    match input {
        CommandPlan::Insert(plan) => plan.subqueries.push(*query),
        CommandPlan::Update(plan) => plan.subqueries.push(*query),
        CommandPlan::Delete(plan) => plan.subqueries.push(*query),
        CommandPlan::Merge(plan) => plan.subqueries.push(*query),
        _ => unreachable!(),
    }
}

#[test]
fn typed_mutation_copies_reject_large_private_arenas_before_allocating() {
    for mut input in commands() {
        add_payload(&mut input, 4 << 20);
        let budget = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("unrelated").unwrap();
        let baseline = budget.used();
        for available in [0, 64, 1024, 16 << 10, 64 << 10] {
            let occupied = budget.reserve(budget.available() - available).unwrap();
            let measured = allocation_counter::measure(|| {
                assert_eq!(
                    copy(&input, &control).unwrap_err().sqlstate(),
                    Some("53200")
                );
            });
            assert!(measured.bytes_total < 128 << 10, "{measured:?}");
            assert_eq!(budget.used(), baseline + occupied.bytes());
            drop(occupied);
        }
        let output = copy(&input, &control).unwrap().finish(&control).unwrap();
        assert_eq!(
            serde_json::to_value(&*output).unwrap(),
            serde_json::to_value(&input).unwrap()
        );
        drop(output);
        assert_eq!(budget.used(), baseline);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn mutation_metadata_and_returning_edits_move_existing_owners_and_retire_only_replaced_fields() {
    for mut input in commands() {
        add_payload(&mut input, 1 << 20);
        let budget = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = Some(copy(&input, &control).unwrap());
        let query_address = arena(output.as_ref().unwrap()).as_ptr();
        let before = budget.used();
        let measured = allocation_counter::measure(|| {
            let value = output
                .take()
                .unwrap()
                .set_target_privilege_subject(
                    RoleReference::Named("view_owner".into())
                        .clone_with_control(&control)
                        .unwrap(),
                    &control,
                )
                .unwrap()
                .retarget_mutation(control.copy_text("public.base").unwrap(), false, &control)
                .unwrap()
                .clear_mutation_ctes(&control)
                .unwrap()
                .map_returning_inputs(&control, |_, projections, _| Ok(projections))
                .unwrap();
            output = Some(value);
        });
        let output = output.unwrap();
        assert_eq!(arena(&output).as_ptr(), query_address);
        assert_eq!(
            before as i64 + measured.bytes_current,
            budget.used() as i64,
            "{measured:?}"
        );
        assert!(measured.bytes_total < 32 << 10, "{measured:?}");
        assert_eq!(output.reserved_bytes(), budget.used());
        let output = output.finish(&control).unwrap();
        let (name, ctes, role, descendants) = match &*output {
            CommandPlan::Insert(plan) => (
                &plan.table,
                &plan.ctes,
                &plan.target_privilege_subject,
                plan.include_descendants,
            ),
            CommandPlan::Update(plan) => (
                &plan.table,
                &plan.ctes,
                &plan.target_privilege_subject,
                plan.include_descendants,
            ),
            CommandPlan::Delete(plan) => (
                &plan.table,
                &plan.ctes,
                &plan.target_privilege_subject,
                plan.include_descendants,
            ),
            CommandPlan::Merge(plan) => (
                &plan.target,
                &plan.ctes,
                &plan.target_privilege_subject,
                plan.include_descendants,
            ),
            _ => unreachable!(),
        };
        assert_eq!(name, "public.base");
        assert!(ctes.is_empty());
        assert!(matches!(role, Some(RoleReference::Named(name)) if name == "view_owner"));
        assert!(!descendants);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn mutation_rule_returning_preserves_first_namespace_and_moves_relation_names() {
    for mut input in commands().into_iter().take(3) {
        add_payload(&mut input, 1 << 20);
        let budget = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = copy(&input, &control)
            .unwrap()
            .record_view_rule_relation("public.first", &control)
            .unwrap();
        let before = budget.used();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                output
                    .record_view_rule_relation("public.second", &control)
                    .unwrap(),
            );
        });
        output = result.unwrap();
        assert_eq!(before as i64 + measured.bytes_current, budget.used() as i64);
        output = output
            .preserve_view_rule_returning("public.first", &control)
            .unwrap();
        let before = budget.used();
        output = output
            .preserve_view_rule_returning("public.second", &control)
            .unwrap();
        assert_eq!(budget.used(), before);
        let output = output.finish(&control).unwrap();
        let (relations, returning) = match &*output {
            CommandPlan::Insert(plan) => (
                &plan.view_rule_relations,
                plan.view_rule_returning.as_ref().unwrap(),
            ),
            CommandPlan::Update(plan) => (
                &plan.view_rule_relations,
                plan.view_rule_returning.as_ref().unwrap(),
            ),
            CommandPlan::Delete(plan) => (
                &plan.view_rule_relations,
                plan.view_rule_returning.as_ref().unwrap(),
            ),
            _ => unreachable!(),
        };
        assert_eq!(relations, &["public.first", "public.second"]);
        assert_eq!(returning.relation, "public.first");
        assert_eq!(
            serde_json::to_value(&returning.subqueries).unwrap(),
            serde_json::to_value(arena(&input)).unwrap()
        );
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn mutation_owner_edits_retire_partial_outputs_on_both_cancellations_and_unwind() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = commands().into_iter().next().unwrap();
        let output = copy(&input, &control).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            output.clear_mutation_ctes(&control).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = commands().into_iter().next().unwrap();
    let output = copy(&input, &control).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = output.map_returning_inputs::<std::iter::Empty<crate::plan::OwnedProjection>>(
            &control,
            |_, projections, _| {
                let _retained = projections;
                panic!("RETURNING callback unwind")
            },
        );
    }))
    .is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn mutation_rule_field_failures_retire_private_storage_and_preserve_siblings() {
    for input in commands().into_iter().take(3) {
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("independent").unwrap();
        let baseline = budget.used();
        for available in [0, 64, 256, 1024] {
            let output = copy(&input, &control)
                .unwrap()
                .record_view_rule_relation("original", &control)
                .unwrap();
            let occupied = budget.reserve(budget.available() - available).unwrap();
            let name = "x".repeat(4 << 20);
            let measured = allocation_counter::measure(|| {
                assert_eq!(
                    output
                        .record_view_rule_relation(&name, &control)
                        .unwrap_err()
                        .sqlstate(),
                    Some("53200")
                );
            });
            assert!(measured.bytes_total < 64 << 10, "{measured:?}");
            assert_eq!(budget.used(), baseline + occupied.bytes());
            assert_eq!(&**sibling, "independent");
            drop(occupied);
        }
        let mut input = input;
        add_payload(&mut input, 2 << 20);
        let output = copy(&input, &control).unwrap();
        let occupied = budget.reserve(budget.available() - (32 << 10)).unwrap();
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                output
                    .preserve_view_rule_returning("first", &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(measured.bytes_total < 64 << 10, "{measured:?}");
        assert_eq!(budget.used(), baseline + occupied.bytes());
        drop(occupied);
        drop(
            copy(&input, &control)
                .unwrap()
                .preserve_view_rule_returning("recovered", &control)
                .unwrap(),
        );
        assert_eq!(budget.used(), baseline);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

mod view_fields;
