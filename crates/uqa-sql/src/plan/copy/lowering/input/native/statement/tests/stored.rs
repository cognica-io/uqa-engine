//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    binding::view_dependencies::visit_stored_plan_relation_references,
    plan::{CommandPlan, StoredStatementObserver},
};
use std::sync::atomic::{AtomicBool, Ordering};
use uqa_core::memory::MemoryReservation;

enum Shape {
    Rows(usize),
    Notification(usize),
}

struct Admitted {
    shape: Shape,
    observed: Arc<AtomicBool>,
    pressure: Option<MemoryReservation>,
}

impl StoredStatementObserver for Admitted {
    async fn plan<W: Work + Send>(
        &mut self,
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        _: &mut W,
    ) -> Result<()> {
        match (&self.shape, plan) {
            (Shape::Rows(count), PlanRef::Command(CommandPlan::Insert(insert))) => {
                assert_eq!(insert.rows.len(), *count);
            }
            (
                Shape::Notification(count),
                PlanRef::Command(CommandPlan::Notify { channel, payload }),
            ) => {
                assert_eq!(channel, "checked");
                assert_eq!(payload, &"α".repeat(*count));
            }
            _ => panic!("unexpected native command"),
        }
        let memory = control.budget().unwrap();
        self.pressure = Some(memory.reserve(memory.available())?);
        self.observed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Never,
    Constructing,
    Retiring,
}

fn run(shape: Shape, steps: usize, stop: Stop) -> (usize, usize) {
    let sql = match shape {
        Shape::Rows(rows) => format!(
            "CREATE FUNCTION public.write_checked() RETURNS void LANGUAGE SQL BEGIN ATOMIC INSERT INTO public.checked(id) VALUES {}; END",
            vec!["(1)"; rows].join(",")
        ),
        Shape::Notification(count) => format!("CREATE RULE signal AS ON INSERT TO public.events DO ALSO NOTIFY checked, '{}'", "α".repeat(count)),
    };
    let mut statements = match crate::compile(&sql).unwrap().remove(0) {
        Statement::CreateFunction(def) => {
            let crate::ast::FunctionBody::Statements(body) = def.body else {
                panic!("SQL-standard routine")
            };
            body
        }
        Statement::CreateRule(rule) => rule.actions,
        _ => panic!("stored statements"),
    };
    assert_eq!(statements.len(), 1);
    let statement = statements.remove(0);
    let memory = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let token = CancellationToken::new();
    let observed = Arc::new(AtomicBool::new(false));
    let mut visitor = Admitted {
        shape,
        observed: observed.clone(),
        pressure: None,
    };
    let mut decoder = ImageDecoder::new(&memory, &token, move |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, &original, input.cancellation);
        UnifiedPlan::visit_stored_statement_with_work(
            &statement,
            &control,
            &mut input,
            &mut visitor,
        )
        .await
    })
    .unwrap();
    let (mut construction, mut retirement) = (0, 0);
    loop {
        if observed.load(Ordering::Relaxed) {
            retirement += 1;
            if stop == Stop::Retiring && retirement == 3 {
                token.cancel();
            }
        } else {
            construction += 1;
            if stop == Stop::Constructing && construction == 3 {
                token.cancel();
            }
        }
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => (),
            Ok(Some(())) => {
                assert!(stop == Stop::Never);
                break;
            }
            Err(error) => {
                assert!(stop != Stop::Never, "{error}");
                assert_eq!(error.sqlstate(), Some("57014"));
                break;
            }
        }
    }
    drop(decoder);
    assert_eq!(memory.used(), 0);
    (construction, retirement)
}

#[test]
fn stored_statement_construction_and_retirement_share_the_original_work_and_allowance() {
    let small = run(Shape::Rows(1), 1, Stop::Never);
    let wide = run(Shape::Rows(4096), 1, Stop::Never);
    let grouped = run(Shape::Rows(4096), 32, Stop::Never);
    assert!(wide.0 > small.0 + 4096 && wide.1 > small.1 + 4096);
    assert!(grouped.0 < wide.0 / 8 && grouped.1 < wide.1 / 8);
    assert_eq!(run(Shape::Rows(4096), 1, Stop::Constructing), (3, 0));
    assert_eq!(run(Shape::Rows(4096), 1, Stop::Retiring).1, 3);
    let small = run(Shape::Notification(1), 1, Stop::Never);
    let wide = run(Shape::Notification(3900), 1, Stop::Never);
    assert!(wide.0 > small.0);
}

struct Original<'a> {
    encoded: &'a [u8],
    check_relations: bool,
}

fn encode(plan: PlanRef<'_>) -> Vec<u8> {
    match plan {
        PlanRef::Query(query) => serde_json::to_vec(query).unwrap(),
        PlanRef::Command(command) => serde_json::to_vec(command).unwrap(),
    }
}

impl StoredStatementObserver for Original<'_> {
    async fn plan<W: Work + Send>(
        &mut self,
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        _: &mut W,
    ) -> Result<()> {
        assert_eq!(encode(plan), self.encoded);
        if self.check_relations {
            visit_stored_plan_relation_references(plan, control, &mut |_, _| Ok(()))?;
        }
        Ok(())
    }
}

#[test]
fn stored_statement_observers_preserve_native_action_payloads_and_utility_diagnostics() {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for sql in [
        "SELECT 1",
        "VALUES (1), (2)",
        "INSERT INTO public.items VALUES (1)",
        "UPDATE public.items SET n = 2",
        "DELETE FROM public.items WHERE n = 1",
        "MERGE INTO public.items i USING public.input s ON i.n = s.n WHEN MATCHED THEN DELETE",
        "NOTIFY checked, 'result α'",
        "LISTEN checked",
        "EXPLAIN SELECT 1",
    ] {
        let statement = crate::compile(sql).unwrap().remove(0);
        let ordinary =
            UnifiedPlan::lower_borrowed_for_rewrite(&statement, &|_: &str| false, &control)
                .unwrap();
        let bytes = encode(ordinary.as_plan_ref());
        for check_relations in [false, true] {
            let mut observer = Original {
                encoded: &bytes,
                check_relations,
            };
            let expected = crate::catalog::work::inline(observer.plan(
                ordinary.as_plan_ref(),
                &control,
                &mut (),
            ));
            match (&statement, check_relations) {
                (Statement::Listen { .. }, true) => assert!(expected
                    .as_ref()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported stored command relation scope")),
                (Statement::Notify { .. } | Statement::Listen { .. }, _) => {
                    assert!(expected.is_ok());
                }
                _ => (),
            }
            let actual =
                crate::catalog::work::inline(UnifiedPlan::visit_stored_statement_with_work(
                    &statement,
                    &control,
                    &mut (),
                    &mut observer,
                ));
            let diagnostic = |e: SQLError| (e.sqlstate().map(str::to_owned), e.to_string());
            assert_eq!(
                actual.map_err(diagnostic),
                expected.map_err(diagnostic),
                "{sql}"
            );
        }
        drop(ordinary);
        assert_eq!(memory.used(), 0, "{sql}");
    }
}
