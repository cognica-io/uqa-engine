//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::semantics::insert_values::single_row_values_query_with_control;
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl, ProductionVec},
    CancellationToken, Value,
};

fn names(
    values: &[&str],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for value in values {
        output.push_produced(control.copy_text(value)?)?;
    }
    Ok(output.finish()?)
}

#[test]
fn set_returning_values_reject_large_command_copies_before_allocation() {
    let mut statement =
        insert("INSERT INTO items(a,b) VALUES(generate_series(1,2),'payload') RETURNING a");
    statement.rows[0][1] = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let memory = MemoryBudget::new(128 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sibling = names(&["sibling"], &control).unwrap();
    let baseline = memory.used();
    let measured = allocation_counter::measure(|| {
        let error = single_row_values_query_with_control(
            &NoRoutines,
            &statement,
            || Ok(Some(names(&[], &control)?)),
            || panic!("explicit target columns"),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 256 << 10, "{measured:?}");
    assert_eq!(memory.used(), baseline);
    assert_eq!(&*sibling, &["sibling"]);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn set_returning_values_keep_exact_command_source_and_default_ownership() {
    for sql in [
        "INSERT INTO items(a,b,c[1]) VALUES(DEFAULT,generate_series(1,2),(SELECT 7)) RETURNING b",
        "INSERT INTO items VALUES(DEFAULT,generate_series(1,2),(SELECT 7)) RETURNING b",
        "INSERT INTO items VALUES(generate_series(1,2),(SELECT 7)) RETURNING b",
    ] {
        let statement = insert(sql);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let sibling = names(&["sibling"], &control).unwrap();
        let baseline = memory.used();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = single_row_values_query_with_control(
                &NoRoutines,
                &statement,
                || Ok(Some(names(&[], &control)?)),
                || Ok(Some(names(&["a", "b", "c"], &control)?)),
                &control,
            )
            .unwrap();
        });
        let result = result.unwrap();
        assert_eq!(
            memory.used() - baseline,
            usize::try_from(measured.bytes_current).unwrap(),
            "{sql}: {measured:?}"
        );
        assert_eq!(result.reserved_bytes(), memory.used() - baseline);
        let CommandPlan::Insert(expanded) = &*result else {
            panic!("INSERT")
        };
        assert!(expanded.rows.is_empty());
        assert_eq!(source_projections(expanded).len(), 2);
        assert_eq!(expanded.subqueries.len(), statement.subqueries.len());
        let RelationalPlan::QueryBlock(block) = &expanded.source.as_ref().unwrap().root else {
            panic!("projection")
        };
        assert_eq!(block.subqueries.len(), statement.subqueries.len());
        assert_ne!(block.subqueries.as_ptr(), expanded.subqueries.as_ptr());
        if sql.contains("DEFAULT") {
            assert_eq!(
                expanded
                    .columns
                    .iter()
                    .map(|target| target.column.as_str())
                    .collect::<Vec<_>>(),
                ["b", "c"]
            );
        } else {
            assert!(expanded.columns.is_empty());
        }
        if sql.contains("c[1]") {
            assert_eq!(expanded.columns[1].indirection.len(), 1);
        }
        drop(result);
        assert_eq!(memory.used(), baseline);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn values_discovery_preserves_callback_demand_and_cleans_partial_failures() {
    let scalar = insert("INSERT INTO items VALUES(1)");
    let statement = insert("INSERT INTO items VALUES(DEFAULT,generate_series(1,2))");
    let large = "x".repeat(4 << 20);
    for failure in 0..4 {
        let memory = MemoryBudget::new(128 << 10);
        let original = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &session);
        let sibling = names(&["sibling"], &control).unwrap();
        let baseline = memory.used();
        assert!(single_row_values_query_with_control(
            &NoRoutines,
            &scalar,
            || panic!("undemanded rules"),
            || panic!("undemanded columns"),
            &control,
        )
        .unwrap()
        .is_none());
        let mut visited = 0;
        let measured = allocation_counter::measure(|| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                single_row_values_query_with_control(
                    &NoRoutines,
                    &statement,
                    || Ok(Some(names(&["unrelated"], &control)?)),
                    || {
                        visited += 1;
                        let mut columns = ProductionVec::new(control);
                        columns.push_produced(control.copy_text("a")?)?;
                        match failure {
                            0 => columns.push_produced(control.copy_text(&large)?)?,
                            1 => original.cancel(),
                            2 => session.cancel(),
                            _ => panic!("column callback unwind"),
                        }
                        Ok(Some(columns.finish()?))
                    },
                    &control,
                )
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
        assert_eq!(visited, 1);
        assert!(measured.bytes_total < 64 << 10, "{measured:?}");
        assert_eq!(memory.used(), baseline);
        let next = CancellationToken::new();
        let next = ProductionControl::new(&memory, &next, &next);
        drop(
            single_row_values_query_with_control(
                &NoRoutines,
                &statement,
                || Ok(Some(names(&[], &next)?)),
                || Ok(Some(names(&["a", "b"], &next)?)),
                &next,
            )
            .unwrap()
            .unwrap(),
        );
        assert_eq!(memory.used(), baseline);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}
