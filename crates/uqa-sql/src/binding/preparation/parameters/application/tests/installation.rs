//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{PlanRef, UnifiedPlan};

fn observe_plan(plan: &MutableExecutablePlan, control: &ProductionControl<'_>) -> InputConstants {
    let mut constants = InputConstants::new(control);
    plan.as_plan_ref()
        .visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |expression| {
                if matches!(
                    expression,
                    ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. }
                ) {
                    constants
                        .11
                        .insert(AnalysisIdentity::from(expression), 0)
                        .unwrap();
                }
            });
        });
    constants
}

fn json(plan: PlanRef<'_>) -> serde_json::Value {
    match plan {
        PlanRef::Query(query) => serde_json::to_value(query).unwrap(),
        PlanRef::Command(command) => serde_json::to_value(command).unwrap(),
    }
}

#[test]
fn parameter_alias_installation_uses_original_observations_across_work_windows() {
    for sql in [
        "WITH a AS (SELECT n) SELECT n, a.n, n AS explicit, (SELECT nested) FROM a",
        "SELECT left_col UNION ALL SELECT right_col",
        "UPDATE t SET n=(SELECT nested) FROM (SELECT source_col) s RETURNING n, n AS explicit",
        "EXPLAIN SELECT n,(SELECT nested)",
    ] {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut expected = source.copy_for_rewrite(&control).unwrap();
        let constants = observe_plan(&expected, &control);
        inline(constants.apply_projection_aliases_owned_from(&mut expected, &control, &mut ()))
            .unwrap();
        let expected_json = json(expected.as_plan_ref());
        drop(expected);
        let expected = expected_json;
        drop(constants);
        let mut plan = source.copy_for_rewrite(&control).unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        let counter = completed.clone();
        let owner = native.clone();
        let original = token.clone();
        let mut input = ImageDecoder::new(&work, &token, |mut reader| async move {
            let mut channel = reader.input();
            let cancellation = channel.cancellation;
            let control = ProductionControl::new(&owner, &original, cancellation);
            let constants = observe_plan(&plan, &control);
            let count = constants.11.values().len();
            let mut work = WorkWindows {
                input: &mut channel,
                completed: counter,
            };
            constants
                .apply_projection_aliases_owned_from(&mut plan, &control, &mut work)
                .await?;
            assert_eq!(constants.11.values().len(), count);
            Ok::<_, SQLError>(plan)
        })
        .unwrap();
        let mut polls = 0;
        loop {
            polls += 1;
            let before = completed.load(Ordering::Relaxed);
            let output = input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                )
                .unwrap();
            assert!(completed.load(Ordering::Relaxed) - before <= 1);
            if let Some(output) = output {
                assert_eq!(json(output.as_plan_ref()), expected, "{sql}");
                drop(output);
                break;
            }
            assert!(polls < 1000);
        }
        assert!(polls > 8);
        drop(input);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
