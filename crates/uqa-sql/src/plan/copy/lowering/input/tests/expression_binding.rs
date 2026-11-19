//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{binding::syntax_sites::expression_syntax_sites, plan::ExpressionPlan, RowSchema};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn stored_default_syntax_preserves_query_slots_without_repeating_aggregate_lookups() {
    let Statement::Select(mut statement) = crate::compile(
        "SELECT ARRAY[(SELECT custom_agg(x) FROM (VALUES (1), (2)) AS t(x)), (SELECT (SELECT 3))]",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let source = statement.projections.remove(0).expr;
    let ordinary = ExpressionPlan::lower_with(source.clone(), &|name: &str| name == "custom_agg");
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let owner = native.clone();
    let original = token.clone();
    let mut decoder = ImageDecoder::new(&work, &token, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&owner, &original, channel.cancellation);
        let lookups = AtomicUsize::new(0);
        let aggregates = |name: &str| {
            if name == "custom_agg" {
                lookups.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            false
        };
        let (runtime, workspace) =
            ExpressionPlan::lower_borrowed_with_work(&source, &aggregates, &control, &mut channel)
                .await?;
        assert_eq!(runtime.queries().len(), 2);
        let checked = lookups.load(Ordering::Relaxed);
        assert!(checked > 0);
        let runtime = runtime.map_query(0, &control, |query| {
            query.set_analyzed_output(Some(RowSchema::default()), &control)
        })?;
        let before = runtime
            .capture_syntax_before_binding(&control, &mut channel)
            .await?;
        let runtime = runtime.finish(&control)?;
        assert_eq!(lookups.load(Ordering::Relaxed), checked);
        assert!(runtime.subqueries[0].analyzed_output.is_some());
        assert!(before.subqueries[0].analyzed_output.is_none());
        assert_eq!(
            serde_json::to_value(&*before).unwrap(),
            serde_json::to_value(&ordinary).unwrap()
        );
        let observed = expression_syntax_sites(&before, &runtime)?;
        let expected = expression_syntax_sites(&ordinary, &runtime)?;
        assert_eq!(observed.values, expected.values);
        assert_eq!(observed.routines.len(), expected.routines.len());
        for (actual, expected) in observed.routines.iter().zip(&expected.routines) {
            assert_eq!(
                (&actual.name, &actual.binding),
                (&expected.name, &expected.binding)
            );
        }
        let before =
            crate::plan::copy::stored_plan::DefaultExpression::new(before, workspace.as_ref());
        let runtime =
            crate::plan::copy::stored_plan::DefaultExpression::new(runtime, workspace.as_ref());
        drop(workspace);
        before.retire(&control, &mut channel).await?;
        runtime.retire(&control, &mut channel).await?;
        Ok::<_, SQLError>(())
    })
    .unwrap();
    let mut calls = 0;
    loop {
        calls += 1;
        if decoder
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(calls < 32_768);
    }
    assert!(calls > 10);
    drop(decoder);
    assert_eq!((native.used(), work.used()), (0, 0));
}
