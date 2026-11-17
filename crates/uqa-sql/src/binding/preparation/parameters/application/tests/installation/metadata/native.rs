//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;

mod interruption;

type Output = (Box<RetainedExecutablePlan>, InputConstants);
type Input = ImageDecoder<Output, SQLError>;

fn syntax(sql: &str) -> Arc<Statement> {
    Arc::new(crate::compile(sql).unwrap().remove(0))
}

fn construct(
    statement: &Arc<Statement>,
    budget: &MemoryBudget,
    work: &MemoryBudget,
    token: &CancellationToken,
) -> Box<RetainedExecutablePlan> {
    let statement = statement.clone();
    let budget = budget.clone();
    let original = token.clone();
    Box::new(
        ImageDecoder::new(work, token, |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(&budget, &original, input.cancellation);
            UnifiedPlan::lower_executable_from_input(
                &statement,
                &|_: &str| false,
                &control,
                &mut input,
            )
            .await
        })
        .unwrap()
        .read(&mut std::io::empty())
        .unwrap(),
    )
}

fn prepare_native(
    statement: &Arc<Statement>,
    replacement: Option<&Arc<Statement>>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    token: &CancellationToken,
) -> (Box<RetainedExecutablePlan>, InputConstants, Observations) {
    let control = ProductionControl::new(native, token, token);
    prepare_plans(
        construct(statement, native, work, token),
        replacement.map(|statement| construct(statement, native, work, token)),
        &control,
    )
}

fn input(
    mut plan: Box<RetainedExecutablePlan>,
    mut constants: InputConstants,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    completed: Arc<AtomicUsize>,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let mut work = WorkWindows {
            input: &mut channel,
            completed,
        };
        let references = constants.11.values().len();
        constants
            .apply_metadata_owned_from(&mut *plan, &control, &mut work)
            .await?;
        assert_eq!(constants.11.values().len(), references);
        Ok((plan, constants))
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Output>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn native_input_metadata_application_preserves_plans_and_original_observations() {
    let replacement_sql = "SELECT renamed,(SELECT nested) FROM generate_series(1,2)";
    let replacement = syntax(replacement_sql);
    for (sql, replace) in [
        ("WITH a AS (SELECT n FROM generate_series(1,2)) SELECT n,(SELECT nested FROM a) FROM a JOIN t ON true", false),
        ("SELECT n FROM ROWS FROM(generate_series(1,2), generate_series(3,4))", false),
        ("SELECT n FROM generate_series(1,2) UNION ALL SELECT m FROM t", false),
        ("UPDATE t SET n=(SELECT nested) FROM generate_series(1,2) RETURNING n", false),
        ("SELECT original FROM old_source", true),
    ] {
        let native = MemoryBudget::new(16 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let replacement_plan = replace.then(|| source(replacement_sql));
        let (mut expected, mut constants, observed) = prepare(&source(sql), replacement_plan.as_ref(), &control);
        inline(constants.apply_metadata_owned_from(&mut *expected, &control, &mut ())).unwrap();
        let expected_json = verify(&mut *expected, &constants, &observed, &control);
        drop((expected, constants, observed));
        let (plan, constants, observed) = prepare_native(&syntax(sql), replace.then_some(&replacement), &native, &work, &token);
        let completed = Arc::new(AtomicUsize::new(0));
        let mut decoder = input(plan, constants, &native, &work, &token, &token, completed.clone());
        for poll in 0..100_000 {
            let before = completed.load(Ordering::Relaxed);
            let output = advance(&mut decoder).unwrap();
            assert!(completed.load(Ordering::Relaxed) - before <= 1);
            if let Some((mut plan, constants)) = output {
                assert_eq!(verify(&mut *plan, &constants, &observed, &control), expected_json, "{sql}");
                drop((plan, constants));
                break;
            }
            assert!(poll < 99_999, "metadata application did not finish");
        }
        drop((decoder, observed));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn native_input_metadata_application_bounds_wide_labels_and_retains_private_maps() {
    let sql = format!(
        "SELECT {} FROM generate_series(1,2)",
        std::iter::repeat_n("selected", 2048)
            .collect::<Vec<_>>()
            .join(",")
    );
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (plan, constants, observed) = prepare_native(&syntax(&sql), None, &native, &work, &token);
    let completed = Arc::new(AtomicUsize::new(0));
    let mut decoder = input(
        plan,
        constants,
        &native,
        &work,
        &token,
        &token,
        completed.clone(),
    );
    let mut polls = 0;
    loop {
        polls += 1;
        let before = completed.load(Ordering::Relaxed);
        let output = advance(&mut decoder).unwrap();
        assert!(completed.load(Ordering::Relaxed) - before <= 1);
        if let Some((mut plan, constants)) = output {
            verify(&mut *plan, &constants, &observed, &control);
            assert_eq!(constants.11.values().len(), 2048);
            let PlanRef::Query(query) = plan.as_plan_ref() else {
                unreachable!()
            };
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!()
            };
            assert_eq!(block.projections.len(), 2048);
            assert!(block
                .projections
                .iter()
                .all(|p| p.alias.as_deref() == Some("selected")));
            drop((plan, constants));
            break;
        }
        assert!(polls < 100_000);
    }
    assert!(polls > 2048);
    drop((decoder, observed));
    assert_eq!((native.used(), work.used()), (0, 0));
}
