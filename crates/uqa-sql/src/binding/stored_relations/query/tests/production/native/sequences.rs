//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ScalarExpr;
use uqa_core::Value;

type Visits = Arc<Mutex<Vec<(String, usize)>>>;
type SequenceInput = ImageDecoder<RetainedExecutablePlan, SQLError>;

fn sequence_input(
    plan: RetainedExecutablePlan,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    visits: Visits,
    fail: Option<&'static str>,
) -> SequenceInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        plan.bind_sequence_references_from_input(
            &|reference, control| {
                if fail == Some(reference) {
                    return Err(SQLError::Routine {
                        sqlstate: "42P01".into(),
                        message: reference.into(),
                    });
                }
                let name = control.format(format_args!("bound.{reference}"))?;
                visits
                    .lock()
                    .unwrap()
                    .push((reference.into(), name.as_ptr() as usize));
                Ok(name)
            },
            &control,
            &mut input,
        )
        .await
    })
    .unwrap()
}
fn step(input: &mut SequenceInput) -> Result<Option<RetainedExecutablePlan>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}
fn finish(input: &mut SequenceInput) -> Result<(RetainedExecutablePlan, usize), SQLError> {
    for polls in 1..1_000_000 {
        if let Some(output) = step(input)? {
            return Ok((output, polls));
        }
    }
    panic!("statement sequence binding stopped progressing")
}

#[test]
fn retained_statement_sequences_preserve_complete_fields_order_and_name_owners() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT nextval('projection'), (SELECT nextval('subquery')) FROM (VALUES (nextval('from'))) t(n) WHERE currval('where') > 0 GROUP BY GROUPING SETS ((nextval('group')), ()) HAVING nextval('having') > 0 ORDER BY nextval('order') LIMIT nextval('limit') OFFSET nextval('offset')",
        "SELECT sum(nextval('argument')) FILTER (WHERE nextval('filter') > 0) OVER w WINDOW w AS (PARTITION BY nextval('partition') ORDER BY nextval('window_order') ROWS BETWEEN nextval('start') PRECEDING AND nextval('end') FOLLOWING)",
        "WITH c AS (SELECT nextval('cte')) SELECT nextval('root') FROM c JOIN (SELECT nextval('derived')) t ON nextval('join') > 0",
        "WITH RECURSIVE c(n) AS (SELECT nextval('cte') UNION ALL SELECT n FROM c WHERE false) CYCLE n SET cy USING path SELECT nextval('root') FROM c",
        "VALUES (nextval('first'), (SELECT nextval('query'))), (currval('second'::regclass), setval('third', 1))",
        "SELECT nextval('left') UNION ALL SELECT nextval('right') LIMIT nextval('limit') OFFSET nextval('offset')",
        "SELECT * FROM ROWS FROM (generate_series(nextval('first'), nextval('second')), generate_series(nextval('third'), nextval('fourth')))",
        "WITH c AS (SELECT nextval('cte')) INSERT INTO base(a[nextval('target')]) VALUES (nextval('row')) ON CONFLICT ((nextval('inference'))) WHERE nextval('infer_predicate') > 0 DO UPDATE SET a[nextval('assignment')]=nextval('value') WHERE nextval('conflict') > 0 RETURNING nextval('returning')",
        "WITH c AS (SELECT nextval('cte')) INSERT INTO base SELECT nextval('source') FROM c RETURNING nextval('returning')",
        "WITH c AS (SELECT nextval('cte')) UPDATE base SET a[nextval('target')]=nextval('value') FROM (SELECT nextval('source')) s WHERE nextval('predicate')>0 RETURNING nextval('returning')",
        "WITH c AS (SELECT nextval('cte')) DELETE FROM base USING (SELECT nextval('source')) s WHERE nextval('predicate')>0 RETURNING nextval('returning')",
        "WITH c AS (SELECT nextval('cte')) MERGE INTO base USING (SELECT nextval('source')) s ON nextval('join')>0 WHEN MATCHED AND nextval('update_when')>0 THEN UPDATE SET a=nextval('update') WHEN NOT MATCHED AND nextval('insert_when')>0 THEN INSERT(a) VALUES (nextval('insert')) WHEN NOT MATCHED BY SOURCE AND nextval('delete_when')>0 THEN DELETE RETURNING nextval('returning')",
        "WITH c AS (UPDATE base SET a=nextval('command_cte') RETURNING a) SELECT nextval('projection') FROM c WHERE nextval('where')>0",
    ] {
        let source = statement(sql);
        let mut expected = UnifiedPlan::lower(source.as_ref().clone());
        let mut expected_order = Vec::new();
        expected.rewrite_scalar_expressions(&mut |node| {
            if let Some(reference) = crate::binding::view_dependencies::sequence_function_reference_mut(node) {
                expected_order.push(reference.clone());
                *reference = format!("bound.{reference}");
            }
        });
        let expected = match expected { UnifiedPlan::Query(value) => serde_json::to_value(value).unwrap(), UnifiedPlan::Command(value) => serde_json::to_value(value).unwrap() };
        let plan = construct(source, &native, &work);
        let visits = Visits::default();
        let mut reader = sequence_input(plan, &native, &work, &token, &token, visits.clone(), None);
        let (actual, polls) = finish(&mut reader).unwrap();
        assert!(polls > 1);
        assert_eq!(observed(&actual), expected, "{sql}");
        let visits = visits.lock().unwrap();
        assert_eq!(visits.iter().map(|(name, _)| name).collect::<Vec<_>>(), expected_order.iter().collect::<Vec<_>>(), "{sql}");
        let mut pointers = Vec::new();
        actual.as_plan_ref().visit_scalar_expressions(&mut |expression| expression.visit(&mut |node| {
            if let ScalarExpr::Literal(Value::Str(name)) = node { if name.starts_with("bound.") { pointers.push(name.as_ptr() as usize); } }
        }));
        assert!(visits.iter().all(|(_, pointer)| pointers.contains(pointer)), "{sql}");
        assert!(native.used() >= actual.reserved_bytes());
        let retirement = allocation_counter::measure(|| drop((actual, reader)));
        assert_eq!(retirement.count_total, 0, "{sql}: {retirement:?}");
        assert_eq!((native.used(), work.used()), (0, 0), "{sql}");
    }
}

#[test]
fn retained_statement_sequences_retire_every_interrupted_structural_prefix() {
    let source = statement("WITH c AS (UPDATE base SET a=nextval('cte') RETURNING a) SELECT nextval('root'), (SELECT nextval('query')) FROM c JOIN (VALUES (nextval('source'))) t(n) ON true");
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let plan = construct(source.clone(), &native, &work);
    let (output, polls) = finish(&mut sequence_input(
        plan,
        &native,
        &work,
        &token,
        &token,
        Visits::default(),
        None,
    ))
    .unwrap();
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let plan = construct(source.clone(), &native, &work);
            let mut input = sequence_input(
                plan,
                &native,
                &work,
                &original,
                &invoking,
                Visits::default(),
                None,
            );
            for _ in 0..prefix {
                assert!(step(&mut input).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(step(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            let retirement = allocation_counter::measure(|| drop(input));
            assert_eq!(retirement.count_total, 0, "{retirement:?}");
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancellation}"
            );
        }
    }
    drop(sibling);
}

#[test]
fn retained_statement_sequence_failures_keep_first_diagnostics_and_sibling_owners() {
    let source = statement("WITH c AS (SELECT nextval('first')) SELECT nextval('missing'), (SELECT nextval('last')) FROM c");
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let sibling = native.reserve(37).unwrap();
    let visits = Visits::default();
    let plan = construct(source.clone(), &native, &work);
    let mut reader = sequence_input(
        plan,
        &native,
        &work,
        &token,
        &token,
        visits.clone(),
        Some("missing"),
    );
    assert_eq!(finish(&mut reader).unwrap_err().sqlstate(), Some("42P01"));
    assert_eq!(
        visits
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    drop(reader);
    assert_eq!((native.used(), work.used()), (37, 0));
    for available in [0, 1, 1024, 4096, 16384] {
        let plan = construct(source.clone(), &native, &work);
        let held = native
            .reserve(native.limit() - native.used() - available)
            .unwrap();
        let expected = held.bytes() + 37;
        let mut reader = sequence_input(
            plan,
            &native,
            &work,
            &token,
            &token,
            Visits::default(),
            None,
        );
        let result = finish(&mut reader);
        if available <= 1 {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            drop(result);
        }
        drop(reader);
        assert_eq!((native.used(), work.used()), (expected, 0));
        drop(held);
    }
    drop(sibling);
}

#[test]
fn retained_statement_sequences_bound_wide_scalar_and_empty_command_fields() {
    let native = MemoryBudget::new(128 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for scalar_fields in [false, true] {
        let source = if scalar_fields {
            statement(&format!(
                "SELECT {}",
                vec!["nextval('wide')"; 4096].join(", ")
            ))
        } else {
            let Statement::Insert(mut source) =
                crate::compile("INSERT INTO base(a) VALUES (DEFAULT)")
                    .unwrap()
                    .remove(0)
            else {
                panic!("insert")
            };
            source.columns[0].indirection = (0..4096)
                .map(|_| crate::ast::AssignmentStep::Field("field".into()))
                .collect();
            Arc::new(Statement::Insert(source))
        };
        let visits = Visits::default();
        let mut reader = sequence_input(
            construct(source, &native, &work),
            &native,
            &work,
            &token,
            &token,
            visits.clone(),
            None,
        );
        let mut polls = 0;
        loop {
            let mut output = None;
            let allocated = allocation_counter::measure(|| output = step(&mut reader).unwrap());
            assert!(
                allocated.count_total <= 16,
                "one-window allocations: {allocated:?}"
            );
            assert!(
                allocated.count_total as i64 - allocated.count_current <= 64,
                "one-window retirement: {allocated:?}"
            );
            polls += 1;
            if let Some(output) = output {
                assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
                break;
            }
            assert!(polls < 200_000);
        }
        assert!(polls > 4096);
        assert_eq!(
            visits.lock().unwrap().len(),
            if scalar_fields { 4096 } else { 0 }
        );
        drop(reader);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_statement_sequences_keep_deep_children_and_failures_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let sibling = native.reserve(37).unwrap();
            for fail in [None, Some("leaf")] {
                let Statement::Select(mut source) =
                    crate::compile("SELECT nextval('leaf')").unwrap().remove(0)
                else {
                    panic!("query")
                };
                for _ in 0..512 {
                    let Statement::Select(mut parent) =
                        crate::compile("WITH c AS (SELECT nextval('cte')) SELECT 1")
                            .unwrap()
                            .remove(0)
                    else {
                        panic!("query")
                    };
                    parent.projections[0].expr = crate::ast::Expr::ScalarSubquery(source);
                    source = parent;
                }
                let visits = Visits::default();
                let plan = construct(Arc::new(Statement::Select(source)), &native, &work);
                let mut reader =
                    sequence_input(plan, &native, &work, &token, &token, visits.clone(), fail);
                match finish(&mut reader) {
                    Ok((output, polls)) => {
                        assert!(fail.is_none());
                        assert!(polls > 512);
                        assert_eq!(visits.lock().unwrap().len(), 513);
                        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
                    }
                    Err(error) => {
                        assert_eq!(fail, Some("leaf"));
                        assert_eq!(error.sqlstate(), Some("42P01"));
                        assert_eq!(visits.lock().unwrap().len(), 512);
                    }
                }
                assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
            drop(sibling);
        })
        .unwrap()
        .join()
        .unwrap();
}
