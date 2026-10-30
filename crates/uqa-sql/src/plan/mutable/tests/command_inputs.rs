//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{CommandPlan, ExpressionPlan, MutableSourcePlan};

fn command(sql: &str) -> CommandPlan {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    *command
}

#[test]
fn values_projection_moves_payloads_and_preserves_query_metadata_and_arenas() {
    for rows in [1, 2] {
        let literal = "x".repeat(4 * 1024 * 1024);
        let sql = if rows == 1 {
            format!("WITH kept AS (SELECT 1) VALUES ('{literal}', (SELECT 7))")
        } else {
            format!(
                "WITH kept AS (SELECT 1) VALUES ('{literal}', (SELECT 7)), ('short', (SELECT 8))"
            )
        };
        let UnifiedPlan::Query(mut input) =
            UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let UnifiedPlan::Query(metadata) = UnifiedPlan::lower(
            crate::compile("WITH kept AS (SELECT 1) SELECT 1")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query metadata")
        };
        input.ctes = metadata.ctes;
        let budget = MemoryBudget::new(8 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = input.copy_for_rewrite(&control).unwrap();
        let RelationalPlan::Values { rows: values, .. } = &input.root else {
            panic!("values")
        };
        let ScalarExpr::Literal(Value::Str(value)) = &values[0][0] else {
            panic!("payload")
        };
        let pointer = value.as_ptr();
        let row_buffer = values.as_ptr();
        let before = budget.used();
        let mut result = None;
        let measured =
            allocation_counter::measure(|| result = Some(input.project_values(&control).unwrap()));
        let result = result.unwrap();
        assert!(measured.bytes_total < 64 * 1024, "{measured:?}");
        assert_eq!(before as i64 + measured.bytes_current, budget.used() as i64);
        assert_eq!(result.reserved_bytes(), budget.used());
        assert_eq!(result.ctes.len(), 1);
        let RelationalPlan::QueryBlock(block) = &result.root else {
            panic!("block")
        };
        assert_eq!(block.projections.len(), 2);
        assert_eq!(block.subqueries.len(), rows);
        let actual = if rows == 1 {
            assert!(block.from.is_none());
            &block.projections[0].expr
        } else {
            let Some(crate::plan::SourcePlan::Values {
                lateral,
                rows: values,
                internal_relation,
                internal_column_types,
                internal_column_collations,
                ..
            }) = &block.from
            else {
                panic!("captured rows")
            };
            assert!(*lateral);
            assert_eq!(values.as_ptr(), row_buffer);
            assert_eq!(internal_column_types, &[None, None]);
            assert_eq!(internal_column_collations, &[None, None]);
            assert!(
                matches!(block.projections[1].expr, ScalarExpr::InternalColumn(column) if column == internal_relation.unwrap().column(1))
            );
            &values[0][0]
        };
        let ScalarExpr::Literal(Value::Str(actual)) = actual else {
            panic!("payload")
        };
        assert_eq!(actual.as_ptr(), pointer);
        assert_eq!(actual, &literal);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn values_projection_rejection_and_cancellation_release_moved_children() {
    let UnifiedPlan::Query(input) = UnifiedPlan::lower(
        crate::compile("VALUES ('payload'), ('second')")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(37).unwrap();
    for token in [&original, &invoking] {
        let owned = input.copy_for_rewrite(&control).unwrap();
        token.cancel();
        assert_eq!(
            owned.project_values(&control).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), sibling.bytes());
        token.reset();
    }
    let owned = input.copy_for_rewrite(&control).unwrap();
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        owned.project_values(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), sibling.bytes() + occupied.bytes());
    drop(occupied);
    drop(
        input
            .copy_for_rewrite(&control)
            .unwrap()
            .project_values(&control)
            .unwrap(),
    );
    assert_eq!(budget.used(), sibling.bytes());
}

#[test]
fn optional_mutation_inputs_reuse_boxes_and_keep_exact_live_owners() {
    let input = command(&format!(
        "UPDATE items SET n = 3 FROM (SELECT '{}' AS payload) source WHERE n = (SELECT 1)",
        "x".repeat(4 * 1024 * 1024)
    ));
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let owned = input.copy_for_rewrite(&control).unwrap();
    let CommandPlan::Update(update) = &*owned else {
        panic!("update")
    };
    let original_box = std::ptr::from_ref(update.source.as_deref().unwrap());
    let added = ExpressionPlan {
        scalar: ScalarExpr::ScalarSubquery(0),
        subqueries: vec![fixture()],
    };
    let before = budget.used();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        let owned = owned
            .map_source_input(&control, |source| {
                Ok(Some(MutableSourcePlan::cross_join(
                    MutableSourcePlan::unit(&control)?,
                    source.unwrap(),
                    true,
                    &control,
                )?))
            })
            .unwrap();
        output = Some(
            owned
                .map_predicate_input(&control, |old, arena| {
                    let added =
                        arena.append_expression(added.copy_for_rewrite(&control)?, &control)?;
                    Ok(Some(OwnedScalarExpr::boolean(
                        [old.unwrap(), added].into_iter(),
                        true,
                        &control,
                    )?))
                })
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(before as i64 + measured.bytes_current, budget.used() as i64);
    assert_eq!(output.reserved_bytes(), budget.used());
    // The source's four MiB payload moves; only the small added expression and constructor records allocate.
    assert!(measured.bytes_total < 256 * 1024, "{measured:?}");
    let CommandPlan::Update(update) = &*output else {
        panic!("update")
    };
    assert_eq!(
        std::ptr::from_ref(update.source.as_deref().unwrap()),
        original_box
    );
    assert_eq!(update.subqueries.len(), 2);
    let ScalarExpr::And(predicates) = update.predicate.as_ref().unwrap() else {
        panic!("conjunction")
    };
    assert!(matches!(predicates[1], ScalarExpr::ScalarSubquery(1)));
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn absent_mutation_sources_and_predicates_can_be_installed_and_retired() {
    for sql in ["UPDATE items SET n = 1", "DELETE FROM items"] {
        let input = command(sql);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = budget.reserve(37).unwrap();
        let owned = input.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut output = None;
        let installed = allocation_counter::measure(|| {
            let owned = owned
                .map_source_input(&control, |source| {
                    assert!(source.is_none());
                    Ok(Some(MutableSourcePlan::unit(&control)?))
                })
                .unwrap();
            output = Some(
                owned
                    .map_predicate_input(&control, |predicate, _| {
                        assert!(predicate.is_none());
                        Ok(Some(
                            ScalarExpr::Literal(Value::Bool(true)).copy_for_rewrite(&control)?,
                        ))
                    })
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(
            before as i64 + installed.bytes_current,
            budget.used() as i64
        );
        assert_eq!(output.reserved_bytes() + sibling.bytes(), budget.used());
        let before = budget.used();
        let mut removed = None;
        let retired = allocation_counter::measure(|| {
            let output = output
                .map_source_input(&control, |source| {
                    drop(source);
                    Ok(None)
                })
                .unwrap();
            removed = Some(
                output
                    .map_predicate_input(&control, |predicate, _| {
                        drop(predicate);
                        Ok(None)
                    })
                    .unwrap(),
            );
        });
        let removed = removed.unwrap();
        assert_eq!(before as i64 + retired.bytes_current, budget.used() as i64);
        assert_eq!(removed.reserved_bytes() + sibling.bytes(), budget.used());
        drop(removed);
        assert_eq!(budget.used(), sibling.bytes());
    }
}

#[test]
fn mutation_input_failure_cancellation_and_unwind_drop_detached_owners() {
    let input = command("DELETE FROM items USING other WHERE n = (SELECT 1)");
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(37).unwrap();
    for token in [&original, &invoking] {
        let owned = input.copy_for_rewrite(&control).unwrap();
        let error = owned
            .map_source_input(&control, |source| {
                token.cancel();
                drop(source);
                Ok(None)
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), sibling.bytes());
        token.reset();
        let owned = input.copy_for_rewrite(&control).unwrap();
        let error = owned
            .map_predicate_input(&control, |predicate, _| {
                token.cancel();
                drop(predicate);
                Ok(None)
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), sibling.bytes());
        token.reset();
    }
    let owned = input.copy_for_rewrite(&control).unwrap();
    assert!(owned
        .map_predicate_input(&control, |_, _| Err(SQLError::Internal("callback".into())))
        .is_err());
    assert_eq!(budget.used(), sibling.bytes());
    let owned = input.copy_for_rewrite(&control).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owned.map_predicate_input(&control, |_, _| panic!("callback unwind"))
    }))
    .is_err());
    assert_eq!(budget.used(), sibling.bytes());
    let owned = input.copy_for_rewrite(&control).unwrap();
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        owned
            .map_predicate_input(&control, |predicate, _| {
                Ok(Some(OwnedScalarExpr::boolean(
                    [
                        predicate.unwrap(),
                        ScalarExpr::Literal(Value::Bool(true)).copy_for_rewrite(&control)?,
                    ]
                    .into_iter(),
                    true,
                    &control,
                )?))
            })
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), sibling.bytes() + occupied.bytes());
}
