//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{QueryMetadataUpdate, QueryPlan, QueryReplacement, RetainedExecutablePlan},
    RowSchema,
};

mod native;
mod observation;
use observation::{observe_queries, Plan};

#[derive(Default)]
struct Observations {
    schemas: Vec<RowSchema>,
    bindings: Vec<usize>,
}

fn source(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn schema(observed: &mut Observations, control: &ProductionControl<'_>) -> RowSchema {
    let label = format!("schema_{}", observed.schemas.len());
    let schema = RowSchema::with_column_metadata_with_control(
        None,
        [(label.as_str(), Some(&crate::ColumnType::Integer), None)],
        control,
    )
    .unwrap();
    observed.schemas.push(schema.clone());
    schema
}

fn prepare(
    source: &UnifiedPlan,
    replacement: Option<&UnifiedPlan>,
    control: &ProductionControl<'_>,
) -> (Box<MutableExecutablePlan>, InputConstants, Observations) {
    prepare_plans(
        Box::new(source.copy_for_rewrite(control).unwrap()),
        replacement.map(|source| Box::new(source.copy_for_rewrite(control).unwrap())),
        control,
    )
}

fn prepare_plans<P: Plan>(
    mut plan: Box<P>,
    mut replacement: Option<Box<P>>,
    control: &ProductionControl<'_>,
) -> (Box<P>, InputConstants, Observations) {
    // The observation's original query root stays stationary across future ownership transfers.
    let selected = replacement.as_mut().unwrap_or(&mut plan);
    let mut constants = observe_plan(selected.as_plan_ref(), control);
    let mut observed = Observations::default();
    observe_queries(&mut **selected, control, &mut |query| {
        constants.8.outputs.insert(
            AnalysisIdentity::from(query),
            schema(&mut observed, control),
        )?;
        Ok(QueryMetadataUpdate::default())
    })
    .unwrap();
    selected.as_plan_ref().visit_sources(&mut |source| {
        constants
            .8
            .sources
            .insert(
                AnalysisIdentity::from(source),
                schema(&mut observed, control),
            )
            .unwrap();
        let mut binding = |binding: &Option<FunctionBinding>| {
            let selected = FunctionBinding::from_stored_json_with_control(
                &format!(
                    r#"{{"name":"selected_{}","argument_types":["integer"]}}"#,
                    observed.bindings.len()
                ),
                control,
            )
            .unwrap();
            observed.bindings.push(selected.name.as_ptr().addr());
            constants
                .7
                .insert(AnalysisIdentity::from(binding), selected)
                .unwrap();
        };
        match source {
            SourcePlan::Function { binding: field, .. } => binding(field),
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    binding(&function.binding);
                }
            }
            _ => (),
        }
    });
    if let Some(replacement) = replacement {
        let PlanRef::Query(query) = plan.as_plan_ref() else {
            unreachable!()
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let PlanRef::Query(replaced) = replacement.as_plan_ref() else {
            unreachable!()
        };
        let output = constants
            .8
            .outputs
            .remove(&AnalysisIdentity::from(replaced))
            .unwrap();
        let replacement = (*replacement).into_replacement();
        constants
            .8
            .outputs
            .insert(AnalysisIdentity::from(query), output)
            .unwrap();
        constants
            .10
            .insert(AnalysisIdentity::from(block.as_ref()), replacement)
            .unwrap();
    }
    (plan, constants, observed)
}

fn verify<P: Plan>(
    plan: &mut P,
    constants: &InputConstants,
    observed: &Observations,
    control: &ProductionControl<'_>,
) -> serde_json::Value {
    assert!(constants.7.is_empty());
    assert!(constants.8.outputs.is_empty());
    assert!(constants.8.sources.is_empty());
    assert!(constants.10.is_empty());
    let check_schema = |schema: &RowSchema| {
        let index: usize = schema.columns()[0]
            .strip_prefix("schema_")
            .unwrap()
            .parse()
            .unwrap();
        assert!(schema.shares_index(&observed.schemas[index]));
    };
    observe_queries(plan, control, &mut |query| {
        check_schema(query.analyzed_output.as_ref().unwrap());
        if let RelationalPlan::QueryBlock(block) = &query.root {
            assert_eq!(query.analyzed_source.is_some(), block.from.is_some());
        }
        let mut pending = query
            .analyzed_source
            .as_deref()
            .into_iter()
            .collect::<Vec<_>>();
        while let Some(source) = pending.pop() {
            check_schema(&source.schema);
            if let Some(children) = &source.children {
                pending.extend([&children.1, &children.0]);
            }
        }
        Ok(QueryMetadataUpdate::default())
    })
    .unwrap();
    plan.as_plan_ref().visit_sources(&mut |source| {
        let check = |binding: &Option<FunctionBinding>| {
            let binding = binding.as_ref().unwrap();
            let index: usize = binding
                .name
                .strip_prefix("selected_")
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(binding.name.as_ptr().addr(), observed.bindings[index]);
        };
        match source {
            SourcePlan::Function { binding, .. } => check(binding),
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    check(&function.binding);
                }
            }
            _ => (),
        }
    });
    json(plan.as_plan_ref())
}

type MetadataOutput = (Box<MutableExecutablePlan>, InputConstants);
type MetadataInput = ImageDecoder<MetadataOutput, SQLError>;

fn input(
    mut plan: Box<MutableExecutablePlan>,
    mut constants: InputConstants,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    completed: Arc<AtomicUsize>,
) -> MetadataInput {
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

fn advance(input: &mut MetadataInput) -> Result<Option<MetadataOutput>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn input_metadata_application_preserves_complete_plans_and_original_observations() {
    let replacement = source("SELECT renamed FROM generate_series(1, 2)");
    for (sql, replace) in [
        ("WITH a AS (SELECT n FROM generate_series(1,2)) SELECT n,(SELECT nested FROM a) FROM a JOIN t ON true", false),
        ("SELECT n FROM ROWS FROM(generate_series(1,2), generate_series(3,4))", false),
        ("SELECT n FROM generate_series(1,2) UNION ALL SELECT m FROM t", false),
        ("UPDATE t SET n=(SELECT nested FROM t) FROM generate_series(1,2) RETURNING n", false),
        ("EXPLAIN SELECT n FROM generate_series(1,2)", false),
        ("SELECT original FROM old_source", true),
    ] {
        let source = source(sql);
        let replacement = replace.then_some(&replacement);
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (mut expected, mut constants, observed) = prepare(&source, replacement, &control);
        inline(constants.apply_metadata_owned_from(&mut *expected, &control, &mut ())).unwrap();
        let expected_json = verify(&mut *expected, &constants, &observed, &control);
        drop((expected, constants, observed));
        let (plan, constants, observed) = prepare(&source, replacement, &control);
        let completed = Arc::new(AtomicUsize::new(0));
        let mut decoder = input(plan, constants, &native, &work, &token, &token, completed.clone());
        loop {
            let before = completed.load(Ordering::Relaxed);
            let output = advance(&mut decoder).unwrap();
            assert!(completed.load(Ordering::Relaxed) - before <= 1);
            if let Some((mut plan, constants)) = output {
                assert_eq!(verify(&mut *plan, &constants, &observed, &control), expected_json, "{sql}");
                drop((plan, constants));
                break;
            }
        }
        drop((decoder, observed));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn input_metadata_application_retires_each_interruption_and_rejects_denied_admission() {
    let source = source("SELECT n FROM generate_series(1,2) JOIN t ON true");
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (plan, constants, observed) = prepare(&source, None, &control);
    let mut complete = input(
        plan,
        constants,
        &native,
        &work,
        &token,
        &token,
        Arc::default(),
    );
    let mut polls = 1;
    while advance(&mut complete).unwrap().is_none() {
        polls += 1;
        assert!(polls < 1000);
    }
    drop((complete, observed));
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let (plan, constants, observed) = prepare(&source, None, &control);
            let mut decoder = input(
                plan,
                constants,
                &native,
                &work,
                &original,
                &invoking,
                Arc::default(),
            );
            for _ in 0..prefix {
                assert!(advance(&mut decoder).unwrap().is_none());
            }
            if cancel == 1 {
                original.cancel();
            } else if cancel == 2 {
                invoking.cancel();
            }
            if cancel != 0 {
                assert!(
                    matches!(advance(&mut decoder), Err(error) if error.sqlstate() == Some("57014"))
                );
            }
            assert_eq!(
                allocation_counter::measure(|| drop((decoder, observed))).count_total,
                0
            );
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        }
    }
    let mut denials = 0;
    for prefix in 0..polls {
        let (plan, constants, observed) = prepare(&source, None, &control);
        let mut decoder = input(
            plan,
            constants,
            &native,
            &work,
            &token,
            &token,
            Arc::default(),
        );
        for _ in 0..prefix {
            assert!(advance(&mut decoder).unwrap().is_none());
        }
        let exhausted = native.reserve(native.available()).unwrap();
        loop {
            match advance(&mut decoder) {
                Ok(None) => (),
                Ok(Some(output)) => {
                    drop(output);
                    break;
                }
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"));
                    denials += 1;
                    break;
                }
            }
        }
        drop((decoder, observed, exhausted));
        assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
    }
    assert!(denials > 0);
}

#[test]
fn input_metadata_application_bounds_wide_installation_on_a_small_stack() {
    let mut source = source("SELECT n");
    let UnifiedPlan::Query(query) = &mut source else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    block.projections.resize(4096, block.projections[0].clone());
    let Ok(Fixture::Source(functions)) = Arc::try_unwrap(functions(4096)) else {
        unreachable!()
    };
    block.from = Some(*functions);
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (plan, constants, observed) = prepare(&source, None, &control);
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
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            let control = ProductionControl::new(&native, &token, &token);
            let mut polls = 0;
            loop {
                polls += 1;
                let before = completed.load(Ordering::Relaxed);
                let output = advance(&mut decoder).unwrap();
                assert!(completed.load(Ordering::Relaxed) - before <= 1);
                if let Some((mut plan, constants)) = output {
                    verify(&mut *plan, &constants, &observed, &control);
                    assert_eq!(constants.11.values().len(), 4096);
                    assert_eq!(observed.bindings.len(), 4096);
                    assert_eq!(
                        allocation_counter::measure(|| drop((plan, constants))).count_total,
                        0
                    );
                    break;
                }
                assert!(polls < 100_000);
            }
            assert!(polls > 8192);
            drop((decoder, observed));
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
