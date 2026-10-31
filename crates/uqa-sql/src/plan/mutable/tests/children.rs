//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{
    CommandScalarSlot, MutableExecutablePlan, QueryChildSlot, SourceChildSlot, SourceScalarSlot,
};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    *query
}

#[test]
fn child_query_edits_retain_parent_containers_and_release_selected_payloads() {
    let fixtures = [
        (
            "WITH child AS (SELECT ARRAY['discarded', 'kept']) SELECT 1",
            QueryChildSlot::Cte(0),
        ),
        (
            "SELECT ARRAY['discarded', 'kept'] UNION ALL SELECT ARRAY['other']",
            QueryChildSlot::Left,
        ),
        (
            "SELECT ARRAY['other'] UNION ALL SELECT ARRAY['discarded', 'kept']",
            QueryChildSlot::Right,
        ),
        (
            "SELECT (SELECT ARRAY['discarded', 'kept'])",
            QueryChildSlot::Subquery(0),
        ),
    ];
    for (sql, slot) in fixtures {
        let source = query(sql);
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = source.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut changed = None;
        let allocation = allocation_counter::measure(|| {
            changed = Some(
                output
                    .map_query_child(slot, &control, |child| {
                        child.map_scalar(QueryScalarSlot::Projection(0), &control, |scalar| {
                            Ok(scalar.select_child(1))
                        })
                    })
                    .unwrap(),
            );
        });
        let changed = changed.unwrap();
        assert_eq!(allocation.count_total, 0, "{sql}");
        assert_eq!(
            before as i64 + allocation.bytes_current,
            budget.used() as i64,
            "{sql}"
        );
        assert_eq!(changed.reserved_bytes(), budget.used());
        drop(changed);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn source_selection_and_attached_query_edits_transfer_only_surviving_constructor_owners() {
    let source = query("SELECT 1 FROM (VALUES (ARRAY['left', 'unused'])) l(v) CROSS JOIN (VALUES (ARRAY['right', 'retained'])) r(v)");
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut changed = None;
    let allocation = allocation_counter::measure(|| {
        changed = Some(
            output
                .map_source(&control, |source| {
                    source
                        .map_child(SourceChildSlot::Right, &control, |right| {
                            right.map_scalar(
                                SourceScalarSlot::Value { row: 0, column: 0 },
                                &control,
                                |scalar| Ok(scalar.select_child(1)),
                            )
                        })?
                        .select_child(SourceChildSlot::Right, &control)
                })
                .unwrap(),
        );
    });
    let changed = changed.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(changed.reserved_bytes(), budget.used());
    drop(changed);
    assert_eq!(budget.used(), 0);

    let source = query("SELECT 1 FROM (SELECT ARRAY['discarded', 'kept']) s(v)");
    let output = source.copy_for_rewrite(&control).unwrap();
    let output = output
        .map_source(&control, |source| {
            source.map_query(&control, |query| {
                query.map_scalar(QueryScalarSlot::Projection(0), &control, |scalar| {
                    Ok(scalar.select_child(1))
                })
            })
        })
        .unwrap();
    drop(output.finish(&control).unwrap());
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_command_copies_and_typed_scalar_edits_preserve_exact_live_storage() {
    let fixtures = [
        (
            "INSERT INTO t VALUES (ARRAY['discarded','kept'])",
            CommandScalarSlot::InsertValue { row: 0, column: 0 },
        ),
        (
            "UPDATE t SET a=ARRAY['discarded','kept']",
            CommandScalarSlot::Assignment(0),
        ),
        (
            "DELETE FROM t RETURNING ARRAY['discarded','kept']",
            CommandScalarSlot::Returning(0),
        ),
        (
            "CALL f(ARRAY['discarded','kept'])",
            CommandScalarSlot::CallArgument(0),
        ),
        (
            "EXECUTE p(ARRAY['discarded','kept'])",
            CommandScalarSlot::ExecuteParameter(0),
        ),
    ];
    for (sql, slot) in fixtures {
        let source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut copied = None;
        let allocation = allocation_counter::measure(|| {
            copied = Some(source.copy_for_rewrite(&control).unwrap());
        });
        let copied = copied.unwrap();
        assert_eq!(
            allocation.bytes_current as usize,
            copied.reserved_bytes(),
            "{sql}"
        );
        let MutableExecutablePlan::Command(command) = copied else {
            panic!("command");
        };
        let command = command
            .map_scalar(slot, &control, |scalar| Ok(scalar.select_child(1)))
            .unwrap();
        let output = MutableExecutablePlan::Command(command)
            .finish(&control)
            .unwrap();
        assert_eq!(output.reserved_bytes(), budget.used(), "{sql}");
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn utility_query_bodies_keep_complete_owning_syntax_through_mutable_copy_and_freeze() {
    for sql in [
        "DECLARE c CURSOR FOR SELECT ARRAY['discarded','kept']",
        "CREATE TABLE t AS SELECT ARRAY['discarded','kept']",
    ] {
        let source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let MutableExecutablePlan::Command(command) = source.copy_for_rewrite(&control).unwrap()
        else {
            panic!("command");
        };
        let command = command
            .map_query_body(&control, |query| {
                query.map_scalar(QueryScalarSlot::Projection(0), &control, |scalar| {
                    Ok(scalar.select_child(1))
                })
            })
            .unwrap();
        let output = MutableExecutablePlan::Command(command)
            .finish(&control)
            .unwrap();
        assert_eq!(output.reserved_bytes(), budget.used());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn owned_query_arena_compaction_preserves_slots_and_releases_discarded_children() {
    let source = query("SELECT (SELECT 'discarded'), (SELECT 'kept')");
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source
        .copy_for_rewrite(&control)
        .unwrap()
        .retain_projections(&control, |index, _| Ok(index == 1))
        .unwrap();
    let RelationalPlan::QueryBlock(block) = &copied.root else {
        panic!("query block");
    };
    let buffer = block.subqueries.as_ptr();
    let capacity = block.subqueries.capacity();
    let before = budget.used();
    let mut changed = None;
    let allocation = allocation_counter::measure(|| {
        changed = Some(copied.prune_subqueries(&control).unwrap());
    });
    let changed = changed.unwrap();
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        panic!("query block");
    };
    assert_eq!(block.subqueries.as_ptr(), buffer);
    assert_eq!(block.subqueries.capacity(), capacity);
    assert_eq!(block.subqueries.len(), 1);
    assert_eq!(block.projections[0].expr, ScalarExpr::ScalarSubquery(0));
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(changed.reserved_bytes(), budget.used());
    drop(changed);
    assert_eq!(budget.used(), 0);
}

#[test]
fn owned_query_arena_rejection_destroys_the_private_query_and_keeps_sibling_allowances() {
    let source = query("SELECT (SELECT 'discarded'), (SELECT 'kept')");
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source
        .copy_for_rewrite(&control)
        .unwrap()
        .retain_projections(&control, |index, _| Ok(index == 1))
        .unwrap();
    let sibling = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        copied.prune_subqueries(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
