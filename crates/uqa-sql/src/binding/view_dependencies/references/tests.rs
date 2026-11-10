//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn references(sql: &str) -> Vec<(String, bool)> {
    let plan = plan(sql);
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let mut references = Vec::new();
    visit_stored_plan_relation_references((&plan).into(), &control, &mut |name, descendants| {
        references.push((name.to_owned(), descendants));
        Ok(())
    })
    .unwrap();
    assert_eq!(memory.used(), 0);
    references.sort();
    references
}

#[test]
fn stored_relations_preserve_nonrecursive_cte_visibility() {
    assert_eq!(
        references(
            "WITH a AS (SELECT * FROM a), b AS (SELECT * FROM a JOIN c USING(id)), c AS (SELECT * FROM public.items) SELECT * FROM b JOIN c USING(id)"
        ),
        vec![
            ("a".into(), true),
            ("c".into(), true),
            ("public.items".into(), true)
        ]
    );
}

#[test]
fn stored_relations_preserve_recursive_siblings_and_qualified_names() {
    assert_eq!(
        references(
            "WITH RECURSIVE a AS (SELECT * FROM b), b AS (SELECT * FROM public.a) SELECT * FROM a"
        ),
        vec![("public.a".into(), true)]
    );
    assert_eq!(
        references(
            "WITH \"A\" AS (SELECT * FROM public.items) SELECT * FROM \"A\" JOIN public.\"A\" USING(id)"
        ),
        vec![("public.\"A\"".into(), true), ("public.items".into(), true)]
    );
}

#[test]
fn stored_relations_restore_outer_scope_after_nested_shadowing() {
    assert_eq!(
        references(
            "WITH a AS (SELECT * FROM public.items) SELECT * FROM (WITH a AS (SELECT * FROM a) SELECT * FROM a) nested JOIN a USING(id) JOIN (SELECT * FROM outside) second USING(id)"
        ),
        vec![("outside".into(), true), ("public.items".into(), true)]
    );
    assert_eq!(
        references(
            "SELECT * FROM (WITH a AS (SELECT * FROM public.items) SELECT * FROM a) nested JOIN a USING(id)"
        ),
        vec![("a".into(), true), ("public.items".into(), true)]
    );
}

#[test]
fn stored_relations_visit_mutation_targets_and_scalar_query_children() {
    assert_eq!(
        references(
            "WITH moved AS (DELETE FROM ONLY public.items RETURNING *) INSERT INTO public.saved SELECT * FROM moved"
        ),
        vec![
            ("public.items".into(), false),
            ("public.saved".into(), true)
        ]
    );
    assert_eq!(
        references(
            "WITH items AS (SELECT id FROM public.other) UPDATE ONLY items SET id=(SELECT id FROM public.next) FROM items s WHERE items.id=s.id"
        ),
        vec![
            ("items".into(), false),
            ("public.next".into(), true),
            ("public.other".into(), true)
        ]
    );
}

#[test]
fn stored_relations_visit_operator_inputs_despite_cte_name_collisions() {
    assert_eq!(
        references(
            "WITH items AS (SELECT * FROM public.other) SELECT * FROM text_similarity_join(items, body, items, body, 0.5)"
        ),
        vec![
            ("items".into(), false),
            ("items".into(), false),
            ("public.other".into(), true)
        ]
    );
}

#[test]
fn stored_relations_retire_workspace_on_callback_quota_and_cancellation_errors() {
    let plan = plan("WITH a AS (SELECT * FROM public.items) SELECT * FROM a");
    let memory = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let error = visit_stored_plan_relation_references((&plan).into(), &control, &mut |_, _| {
        Err(SQLError::Internal("reference rejected".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("reference rejected"));
    assert_eq!(memory.used(), 0);
    let empty = MemoryBudget::new(0);
    let denied = ProductionControl::new(&empty, &original, &invoking);
    assert!(
        visit_stored_plan_relation_references((&plan).into(), &denied, &mut |_, _| Ok(())).is_err()
    );
    assert_eq!(empty.used(), 0);
    invoking.cancel();
    assert!(
        visit_stored_plan_relation_references((&plan).into(), &control, &mut |_, _| panic!(
            "cancelled input must not visit"
        ))
        .is_err()
    );
    let active = CancellationToken::new();
    original.cancel();
    let control = ProductionControl::new(&memory, &original, &active);
    assert!(
        visit_stored_plan_relation_references((&plan).into(), &control, &mut |_, _| panic!(
            "cancelled owner must not visit"
        ))
        .is_err()
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_relations_borrow_large_payloads_and_reject_unsupported_commands() {
    let payload = "x".repeat(1 << 20);
    let plan = plan(&format!("SELECT '{payload}' FROM public.items"));
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let mut visits = 0;
    let allocation = allocation_counter::measure(|| {
        visit_stored_plan_relation_references((&plan).into(), &control, &mut |name, _| {
            assert_eq!(name, "public.items");
            visits += 1;
            Ok(())
        })
        .unwrap();
    });
    assert_eq!(visits, 1);
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(memory.used(), 0);
    let command = UnifiedPlan::lower(crate::compile("DROP TABLE public.items").unwrap().remove(0));
    assert!(
        visit_stored_plan_relation_references((&command).into(), &control, &mut |_, _| Ok(()))
            .is_err()
    );
}
