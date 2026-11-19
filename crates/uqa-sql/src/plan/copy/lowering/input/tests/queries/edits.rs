//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{
    scalar::{ScalarEdit, ScalarEditIdentity, ScalarInputWork},
    OwnedScalarExpr,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};
use uqa_core::memory::MemoryReservation;

#[derive(Debug)]
struct Editor {
    sites: Option<BTreeMap<usize, usize>>,
    visited: Vec<usize>,
    calls: Arc<AtomicUsize>,
    fail_at: Option<usize>,
    _memory: Option<MemoryReservation>,
}

impl Editor {
    fn new(
        plan: PlanRef<'_>,
        capture: bool,
        calls: Arc<AtomicUsize>,
        fail_at: Option<usize>,
        control: &ProductionControl<'_>,
    ) -> Self {
        let sites = capture.then(|| {
            let mut sites = BTreeMap::new();
            plan.visit_scalar_expressions(&mut |expression| {
                expression.visit(&mut |expression| {
                    let ordinal = sites.len();
                    assert!(sites.insert(Self::identity(expression), ordinal).is_none());
                });
            });
            sites
        });
        Self {
            sites,
            visited: Vec::new(),
            calls,
            fail_at,
            _memory: control.reserve(53).unwrap(),
        }
    }
}

impl ScalarEditIdentity for Editor {
    type Identity = usize;
    const BEFORE_CHILDREN: bool = false;

    fn identity(expression: &ScalarExpr) -> usize {
        std::ptr::from_ref(expression).addr()
    }
}

impl<W: ScalarInputWork> ScalarEdit<W> for Editor
where
    W::Error: Into<SQLError>,
{
    async fn apply(
        &mut self,
        identity: usize,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr> {
        work.checkpoint().await.map_err(Into::into)?;
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let ordinal = if let Some(sites) = &mut self.sites {
            let ordinal = sites.remove(&identity).expect("original scalar identity");
            self.visited.push(ordinal);
            ordinal
        } else {
            call
        };
        if self.fail_at == Some(call) {
            return Err(SQLError::Internal("selected scalar edit failed".into()));
        }
        if matches!(&*expression, ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } if name == "marker")
        {
            return Ok(OwnedScalarExpr::literal(
                control.finish(
                    Value::Int((ordinal + 1) as i64),
                    control.empty_reservation(),
                )?,
                control,
            )?);
        }
        Ok(expression)
    }
}

type EditedInput = ImageDecoder<(RetainedExecutablePlan, Editor), SQLError>;

fn edited_input(
    source: Arc<Statement>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    calls: Arc<AtomicUsize>,
    (capture, fail_at, deny_edit): (bool, Option<usize>, bool),
) -> EditedInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let plan = UnifiedPlan::lower_executable_from_input(
            &source,
            &NoRegisteredAggregates,
            &control,
            &mut channel,
        )
        .await?;
        let editor = Editor::new(plan.as_plan_ref(), capture, calls, fail_at, &control);
        let _held = if deny_edit {
            Some(native.reserve(native.available())?)
        } else {
            None
        };
        plan.edit_scalars_from_input(editor, &control, &mut channel)
            .await
    })
    .unwrap()
}

fn step(input: &mut EditedInput) -> Result<Option<(RetainedExecutablePlan, Editor)>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn retained_statement_edits_preserve_original_scalar_identities_and_order() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT marker, (SELECT marker) FROM (VALUES (1)) q(marker) WHERE marker > 0",
        "SELECT a.marker FROM (VALUES (true)) a(marker) JOIN (VALUES (1)) b(n) ON a.marker",
        "SELECT a.marker FROM (VALUES (true)) a(marker) JOIN ((VALUES (true)) b(marker) JOIN (VALUES (1)) c(n) ON b.marker) ON NOT a.marker",
        "WITH q AS (SELECT marker FROM (VALUES (1)) s(marker)) SELECT marker FROM q UNION ALL SELECT marker FROM q ORDER BY 1 LIMIT 2",
        "WITH RECURSIVE q(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM q WHERE n<2) CYCLE n SET flag TO true DEFAULT false USING path SELECT marker FROM (VALUES (1)) s(marker)",
        "SELECT sum(marker) FILTER (WHERE marker > 0) OVER w FROM (VALUES (1)) s(marker) WINDOW w AS (ORDER BY marker ROWS 1 PRECEDING)",
        "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET v=(SELECT marker) RETURNING marker",
        "UPDATE t SET a[marker]=(SELECT marker) FROM (VALUES (1)) s(marker) WHERE marker > 0 RETURNING marker",
        "DELETE FROM t USING (VALUES (1)) s(marker) WHERE t.id=marker RETURNING marker",
        "MERGE INTO t USING (VALUES (1)) s(marker) ON t.id=s.marker WHEN MATCHED THEN UPDATE SET v=s.marker WHEN NOT MATCHED THEN INSERT (id) VALUES (s.marker) RETURNING t.id",
    ] {
        let source = Arc::new(statement(sql));
        let control = ProductionControl::new(&native, &token, &token);
        let mut expected = UnifiedPlan::lower_borrowed_for_rewrite(&source, &NoRegisteredAggregates, &control).unwrap();
        let mut expected_editor = Editor::new(expected.as_plan_ref(), true, Arc::new(AtomicUsize::new(0)), None, &control);
        expected.transform_scalar_nodes_in_place(&control, &mut |identity, expression| {
            crate::catalog::work::inline(expected_editor.apply(identity.as_ptr().addr(), expression, &control, &mut ()))
        }).unwrap();
        let input = edited_input(source, &native, &work, &token, &token, Arc::new(AtomicUsize::new(0)), (true, None, false));
        let (actual, editor) = input.read(&mut std::io::empty()).unwrap();
        assert_eq!(json(actual.as_plan_ref()), json(expected.as_plan_ref()), "{sql}");
        assert_eq!(editor.visited, expected_editor.visited, "{sql}");
        assert!(editor.sites.as_ref().unwrap().is_empty(), "{sql}");
        assert!(expected_editor.sites.as_ref().unwrap().is_empty(), "{sql}");
        drop((actual, editor, expected, expected_editor));
        assert_eq!((native.used(), work.used()), (0, 0), "{sql}");
    }
}

#[test]
fn retained_statement_edits_retire_each_interrupted_prefix_and_failed_child() {
    let source = Arc::new(statement("SELECT a.marker, (SELECT 7) FROM (VALUES (true)) a(marker) JOIN (VALUES (1)) b(n) ON a.marker"));
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = edited_input(
        source.clone(),
        &native,
        &work,
        &token,
        &token,
        Arc::new(AtomicUsize::new(0)),
        (true, None, false),
    );
    let mut polls = 0;
    loop {
        polls += 1;
        if let Some(output) = step(&mut input).unwrap() {
            drop(output);
            break;
        }
        assert!(polls < 10000);
    }
    drop(input);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = edited_input(
                source.clone(),
                &native,
                &work,
                &original,
                &invoking,
                Arc::new(AtomicUsize::new(0)),
                (true, None, false),
            );
            for _ in 0..prefix {
                assert!(step(&mut input).unwrap().is_none());
            }
            match cancel {
                1 => original.cancel(),
                2 => invoking.cancel(),
                _ => (),
            }
            if cancel != 0 {
                assert_eq!(step(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
            assert_eq!(
                (native.used(), work.used()),
                (sibling.bytes(), 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let input = edited_input(
        source,
        &native,
        &work,
        &token,
        &token,
        calls.clone(),
        (true, Some(3), false),
    );
    assert_eq!(
        input.read(&mut std::io::empty()).unwrap_err().to_string(),
        "internal error: selected scalar edit failed"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
}

#[test]
fn retained_statement_edits_bound_wide_work_and_deep_polling() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            for deep in [false, true] {
                let mut source = blocks::query("SELECT marker");
                let expected = if deep {
                    for _ in 0..512 {
                        let mut parent = blocks::query("SELECT 1");
                        parent.projections[0].expr = Expr::ScalarSubquery(Box::new(source));
                        source = parent;
                    }
                    513
                } else {
                    source.projections = vec![source.projections[0].clone(); 2048];
                    2048
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let mut input = edited_input(
                    Arc::new(Statement::Select(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                    calls.clone(),
                    (false, None, false),
                );
                let mut polls = 0;
                loop {
                    polls += 1;
                    assert!(polls < 1_000_000, "deep={deep}");
                    let before = calls.load(Ordering::Relaxed);
                    let output = step(&mut input).unwrap();
                    assert!(calls.load(Ordering::Relaxed) - before <= 1);
                    if let Some(output) = output {
                        assert_eq!(calls.load(Ordering::Relaxed), expected);
                        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
                        break;
                    }
                }
                drop(input);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_statement_edit_admission_retires_original_plan_and_editor() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let sibling = native.reserve(37).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let input = edited_input(
        Arc::new(statement("SELECT marker, (SELECT marker)")),
        &native,
        &work,
        &token,
        &token,
        calls.clone(),
        (true, None, true),
    );
    assert_eq!(
        input.read(&mut std::io::empty()).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
}
