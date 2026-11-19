//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{EventEnableMode, Expr, FunctionBinding},
    catalog::events::RuleConditionBinding,
    plan::UnifiedPlan,
    ScalarExpr,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    Value,
};

fn trigger() -> StoredTrigger {
    let Statement::CreateTrigger(definition) = crate::compile("CREATE TRIGGER report BEFORE INSERT ON items FOR EACH ROW WHEN (condition_input(NEW.v)) EXECUTE FUNCTION durable_handler('payload')").unwrap().remove(0) else { panic!("trigger") };
    StoredTrigger {
        definition,
        function_object_id: Some([1; 16]),
        enabled: EventEnableMode::Always,
        object_id: None,
        constraint_name: None,
        catalog_oid: None,
        constraint_catalog_oid: None,
    }
}

fn rule(sql: &str) -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("rule")
    };
    StoredRule {
        definition,
        enabled: EventEnableMode::Always,
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: None,
    }
}

type Calls = Vec<(String, StoredFunctionSite)>;

fn observe<T: EventRuntimeCalls + Send + 'static>(
    event: T,
    steps: usize,
    cancel: bool,
) -> (Result<Calls, SQLError>, usize) {
    let memory = MemoryBudget::new(8 << 20);
    let invoking = CancellationToken::new();
    let original = CancellationToken::new();
    let token = original.clone();
    let mut decoder = ImageDecoder::new(&memory, &invoking, move |mut reader| async move {
        let mut calls = Vec::new();
        event
            .runtime_definition()
            .visit_runtime_calls(&token, &mut reader.input(), &mut |name, site| {
                calls.push((name.to_owned(), site));
                Ok(())
            })
            .await?;
        Ok::<_, SQLError>(calls)
    })
    .unwrap();
    let mut polls = 0;
    let result = loop {
        polls += 1;
        if cancel && polls == 64 {
            original.cancel();
        }
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => (),
            other => break other.map(Option::unwrap),
        }
    };
    drop(decoder);
    assert_eq!(memory.used(), 0);
    (result, polls)
}

#[test]
fn event_calls_preserve_the_selected_predicate_and_action_placements() {
    use StoredFunctionSite::{Expression, From, ScalarOnly, Window};
    let mut source = trigger();
    assert_eq!(
        observe(source.clone(), 1, false).0.unwrap(),
        [("condition_input".into(), ScalarOnly)]
    );
    let Expr::Func { name, binding, .. } = source.definition.when.as_mut().unwrap() else {
        panic!("condition")
    };
    *binding = Some(FunctionBinding::builtin_by_name(name, None));
    assert!(observe(source, 1, false).0.unwrap().is_empty());

    let notify = rule("CREATE RULE report AS ON INSERT TO items DO ALSO NOTIFY reported");
    assert!(observe(notify, 1, false).0.unwrap().is_empty());
    let legacy = rule("CREATE RULE report AS ON INSERT TO items WHERE EXISTS (SELECT window_input() OVER () FROM table_input(NEW.v)) DO NOTHING");
    let calls = observe(legacy, 1, false).0.unwrap();
    assert!(calls.contains(&("window_input".into(), Window)));
    assert!(calls.contains(&("table_input".into(), From)));
    assert_eq!(calls.len(), 2);

    let mut source = rule("CREATE RULE report AS ON INSERT TO items WHERE source_predicate(NEW.v) DO ALSO INSERT INTO audit VALUES (action_input(NEW.v))");
    assert_eq!(
        observe(source.clone(), 1, false).0.unwrap(),
        [
            ("source_predicate".into(), Expression),
            ("action_input".into(), Expression)
        ]
    );
    let mut plan = ExpressionPlan::lower(source.definition.condition.as_ref().unwrap().clone());
    let ScalarExpr::Func { name, binding, .. } = &mut plan.scalar else {
        panic!("condition")
    };
    *binding = Some(FunctionBinding::builtin_by_name(name, None));
    let UnifiedPlan::Query(subquery) = UnifiedPlan::lower(
        crate::compile("SELECT window_input() OVER () FROM table_input(1)")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    plan.subqueries.push(*subquery);
    source.condition_plan = Some(plan);
    source.condition_binding = Some(RuleConditionBinding::for_event(
        &["v".into()],
        source.definition.event,
    ));
    assert_eq!(
        observe(source, 1, false).0.unwrap(),
        [
            ("window_input".into(), Window),
            ("table_input".into(), From),
            ("action_input".into(), Expression)
        ]
    );
}

#[test]
fn late_trigger_predicate_calls_yield_and_release_original_input_on_cancellation() {
    let mut event = trigger();
    let tail = event.definition.when.take().unwrap();
    let mut predicates = vec![Expr::Literal(Value::Null); 4096];
    predicates.push(tail);
    event.definition.when = Some(Expr::And(predicates));
    let (single, polls) = observe(event.clone(), 1, false);
    assert_eq!(
        single.unwrap(),
        [("condition_input".into(), StoredFunctionSite::ScalarOnly)]
    );
    let (coarse, batches) = observe(event.clone(), 32, false);
    assert_eq!(
        coarse.unwrap(),
        [("condition_input".into(), StoredFunctionSite::ScalarOnly)]
    );
    assert!(polls > 4096 && polls > batches);
    let (cancelled, polls) = observe(event, 1, true);
    assert_eq!(cancelled.unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(polls, 64);
}
