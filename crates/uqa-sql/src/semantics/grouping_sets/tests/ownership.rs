//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::production::{expression_identity_with_control, GroupingKey};
use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn mutable_grouping_selection_retires_discarded_sets_and_analysis_storage() {
    let UnifiedPlan::Query(mut source) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block");
    };
    block.group_distinct = true;
    block.grouping_sets = ["same", "same", "different"]
        .map(|value| vec![ScalarExpr::Literal(Value::Str(value.repeat(2048)))])
        .into();
    source.analyzed_grouping = Some(AnalyzedGroupingSets {
        original_count: 3,
        retained: vec![0, 2],
    });
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut changed = None;
    let allocation = allocation_counter::measure(|| {
        changed = Some(copied.apply_grouping(&control).unwrap());
    });
    let changed = changed.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(changed.reserved_bytes(), budget.used());
    assert!(changed.analyzed_grouping.is_none());
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        panic!("query block");
    };
    assert!(!block.group_distinct);
    assert_eq!(block.grouping_sets.len(), 2);
    for (set, value) in block.grouping_sets.iter().zip(["same", "different"]) {
        assert_eq!(set, &[ScalarExpr::Literal(Value::Str(value.repeat(2048)))]);
    }
    drop(changed);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_grouping_identity_keeps_only_serialized_output_until_release() {
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = input();
    for sql in [
        "SELECT id, t.id, n + 1, -42, coalesce(n,1), lower(NULL), NULL::text, '3'::integer, CASE WHEN n>0 THEN 1 ELSE 2 END FROM t",
        "SELECT sum(n) FILTER (WHERE n>0) OVER (PARTITION BY id ORDER BY n ROWS BETWEEN 1 PRECEDING AND 2 FOLLOWING) FROM t",
        "SELECT ROW(1,'a'), ARRAY[1,2], n BETWEEN 1 AND 2, n IN (1,2), n IS NULL, NOT n>0 FROM t",
    ] {
        let block = block(sql);
        for projection in &block.projections {
            let expression = crate::bind_type_introspection_with_resolver(projection.expr.clone(), &schema, &[], &Catalog);
            let normalized = normalize_expression(&Catalog, expression, &schema, &[]).unwrap();
            let expected = serde_json::to_vec(&normalized).unwrap();
            let mut output = None;
            let allocations = allocation_counter::measure(|| {
                output = Some(expression_identity_with_control(&Catalog, &projection.expr, &schema, &[], &control).unwrap());
            });
            let output = output.unwrap();
            assert_eq!(&*output, &expected, "{sql}");
            assert_eq!(budget.used(), output.reserved_bytes());
            assert_eq!(allocations.bytes_current as usize, budget.used());
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn grouped_column_slots_preserve_aliases_correlations_and_expression_identity() {
    let source = RowSchema::with_identities(
        vec!["id".into(), "n".into(), "outer_id".into()],
        vec![
            crate::ColumnIdentity::qualified("t", "id"),
            crate::ColumnIdentity::qualified("t", "n"),
            crate::ColumnIdentity::qualified("o", "outer_id"),
        ],
        vec![Some(ColumnType::Integer); 3],
    );
    let source = RowSchema::with_identity_aliases(
        &source,
        &[(crate::ColumnIdentity::unqualified("alias_id"), 0)],
    );
    let outer = RowSchema::with_types(vec!["outer_id".into()], vec![Some(ColumnType::Integer)]);
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let column = ScalarExpr::Column("id".into());
    let qualified = ScalarExpr::QualifiedColumn {
        qualifier: "t".into(),
        column: "id".into(),
    };
    let alias = ScalarExpr::Column("alias_id".into());
    for expression in [&column, &qualified, &alias, &ScalarExpr::Position(0)] {
        let count = allocation_counter::measure(|| {
            let key = GroupingKey::new(&Catalog, expression, &source, &[], &control).unwrap();
            assert_eq!(key, GroupingKey::Position(0));
        });
        assert_eq!(count.count_total, 0);
    }
    for sql in [
        "SELECT alias_id, t.id::integer, o.outer_id, count(*) FROM t GROUP BY t.id",
        "SELECT id, alias_id, count(*) FROM t GROUP BY t.id::integer",
        "SELECT alias_id + 1, o.outer_id, count(*) FROM t GROUP BY t.id + 1",
    ] {
        validate_grouped_expressions(&Catalog, &block(sql), &source, Some(&outer), &[]).unwrap();
    }
    let error = validate_grouped_expressions(
        &Catalog,
        &block("SELECT t.n, t.id FROM t GROUP BY o.outer_id"),
        &source,
        Some(&outer),
        &[],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42803"));
    assert!(error.to_string().contains("column \"t.n\""));
    token.cancel();
    let error = GroupingKey::new(&Catalog, &column, &source, &[], &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn controlled_distinct_grouping_keeps_first_normalized_set_and_only_output_capacity() {
    let original = block("SELECT n + 1 AS shifted FROM t GROUP BY DISTINCT GROUPING SETS ((shifted,n),(n,n+1),(n),(n),())");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = input();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(
            analyze_distinct_grouping_with_control(
                &Catalog,
                &original,
                &schema,
                None,
                &[],
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(output.retained, [0, 2, 4]);
    assert_eq!(output.original_count, 5);
    assert_eq!(budget.used(), output.reserved_bytes());
    assert_eq!(allocations.bytes_current as usize, budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn grouping_production_rejects_large_payload_and_observes_each_cancellation_owner() {
    let source = ScalarExpr::Literal(Value::Bytes(vec![7; 4 << 20]));
    for mode in 0..3 {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        match mode {
            1 => original.cancel(),
            2 => invoking.cancel(),
            _ => {}
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = expression_identity_with_control(
            &Catalog,
            &source,
            &RowSchema::default(),
            &[],
            &control,
        )
        .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
        let error = super::super::serialization::identity(&source, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
    }
}

fn grouped_input(
    block: QueryBlockPlan,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    selected: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> uqa_core::image_io::ImageDecoder<(), SQLError> {
    let budget = native.clone();
    let original = original.clone();
    let selected = selected.clone();
    uqa_core::image_io::ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&budget, &original, channel.cancellation);
        let schema = input();
        super::super::validate_grouped_expressions_with_inputs(
            &Catalog,
            &block,
            &schema,
            None,
            &[],
            block.grouping_sets.iter().flatten().inspect(|_| {
                selected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
            &mut crate::catalog::work::ReferenceWork {
                control: &control,
                work: &mut channel,
            },
        )
        .await
    })
    .unwrap()
}

#[test]
fn grouped_validation_schedules_identities_and_projection_errors_in_original_order() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_core::image_io::ImageDecodeWindow;

    let native = MemoryBudget::new(16 << 10);
    let work = MemoryBudget::new(1 << 20);
    let group = |count| {
        std::iter::repeat_n("(n)", count)
            .collect::<Vec<_>>()
            .join(",")
    };
    let complete = |width, target: &str, steps| {
        let query = block(&format!(
            "SELECT {target} FROM t GROUP BY DISTINCT GROUPING SETS ({})",
            group(width)
        ));
        let expected = validate_grouped_expressions(&Catalog, &query, &input(), None, &[])
            .err()
            .map(|error| (error.sqlstate().unwrap().to_owned(), error.to_string()));
        let selected = Arc::new(AtomicUsize::new(0));
        let token = CancellationToken::new();
        let mut job = grouped_input(query, &native, &work, &token, &token, &selected);
        let mut calls = 0;
        let seen = loop {
            calls += 1;
            let before = selected.load(Ordering::Relaxed);
            let result = job.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps });
            // The iterator may have selected the next expression when its first checkpoint yields.
            assert!(selected.load(Ordering::Relaxed) - before <= steps + usize::from(calls == 1));
            match result {
                Ok(None) => (),
                Ok(Some(())) => break None,
                Err(error) => {
                    break Some((error.sqlstate().unwrap().to_owned(), error.to_string()))
                }
            }
        };
        assert_eq!(seen, expected);
        assert_eq!(selected.load(Ordering::Relaxed), width);
        drop(job);
        assert_eq!((native.used(), work.used()), (0, 0));
        calls
    };
    let small = complete(2, "count(*)", 1);
    let wide = complete(32, "count(*)", 1);
    assert!(
        wide >= small + 60,
        "one checkpoint per additional identity and its release: {small}/{wide}"
    );
    assert!(complete(32, "count(*)", 8) < wide / 4);
    let first = complete(32, "id,n,n,n,n,n,n,n", 1);
    let last = complete(32, "n,n,n,n,n,n,n,id", 1);
    eprintln!("grouped validation, one-step calls: 2 sets {small}, 32 sets {wide}, first/last error {first}/{last}");
    assert_eq!(last - first, 7);
    for interrupted in 0..3 {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let selected = Arc::new(AtomicUsize::new(0));
        let query = block(&format!(
            "SELECT count(*) FROM t GROUP BY DISTINCT GROUPING SETS ({})",
            group(32)
        ));
        let mut job = grouped_input(query, &native, &work, &original, &invoking, &selected);
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        while selected.load(Ordering::Relaxed) < 8 {
            assert!(job
                .advance(&mut std::io::empty(), window)
                .unwrap()
                .is_none());
        }
        assert!(native.used() > 0);
        let pressure = match interrupted {
            0 => {
                original.cancel();
                None
            }
            1 => {
                invoking.cancel();
                None
            }
            _ => Some(native.reserve(native.limit() - native.used()).unwrap()),
        };
        let error = job.advance(&mut std::io::empty(), window).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if interrupted == 2 { "53200" } else { "57014" })
        );
        drop((job, pressure));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn distinct_grouping_selection_schedules_each_normalized_set_and_its_keys() {
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

    let progress = |width: usize, steps, interruption| {
        let keys = std::iter::repeat_n("n", width)
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id AS item FROM t GROUP BY DISTINCT GROUPING SETS (({keys}),(n),(item),(),())"
        );
        let query = block(&sql);
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let budget = native.clone();
        let owner = original.clone();
        let mut job = ImageDecoder::new(&work, &invoking, |mut reader| async move {
            let mut channel = reader.input();
            let control = ProductionControl::new(&budget, &owner, channel.cancellation);
            let schema = input();
            super::super::analyze_distinct_grouping_from(
                &Catalog,
                &query,
                &schema,
                None,
                &[],
                &mut crate::catalog::work::ReferenceWork {
                    control: &control,
                    work: &mut channel,
                },
            )
            .await
        })
        .unwrap();
        let mut calls = 0;
        let mut pressure = None;
        loop {
            calls += 1;
            if calls == 12 {
                match interruption {
                    Some(0) => original.cancel(),
                    Some(1) => invoking.cancel(),
                    Some(2) => {
                        pressure = Some(native.reserve(native.limit() - native.used()).unwrap());
                    }
                    _ => (),
                }
            }
            match job.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
                Ok(None) => (),
                Ok(Some(output)) => {
                    assert!(interruption.is_none());
                    assert_eq!(output.original_count, 5);
                    assert_eq!(output.retained, [0, 2, 3]);
                    assert_eq!(native.used(), output.reserved_bytes());
                    break;
                }
                Err(error) => {
                    assert_eq!(
                        error.sqlstate(),
                        Some(if interruption == Some(2) {
                            "53200"
                        } else {
                            "57014"
                        })
                    );
                    assert!(interruption.is_some());
                    break;
                }
            }
        }
        drop((job, pressure));
        assert_eq!((native.used(), work.used()), (0, 0));
        calls
    };
    let small = progress(2, 1, None);
    let wide = progress(64, 1, None);
    eprintln!("distinct sets, one-step calls: 2 keys {small}, 64 keys {wide}");
    assert!(wide >= small + 62 * 2);
    assert!(progress(64, 32, None) < wide / 8);
    for interruption in 0..3 {
        progress(64, 1, Some(interruption));
    }
}
