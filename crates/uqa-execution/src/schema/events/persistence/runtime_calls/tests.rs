//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::{json, Value};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::{
    ast::{EventEnableMode, FunctionBinding},
    catalog::events::{RuleConditionBinding, StoredRule, StoredTrigger},
    plan::ExpressionPlan,
    ScalarExpr, Statement,
};

type Calls = Vec<(String, StoredFunctionSite)>;

#[derive(Clone, Copy)]
enum Scope {
    Complete,
    Cancel,
    Exhaust,
}

fn read(
    catalog: Catalog,
    record: &Value,
    memory: &MemoryBudget,
    steps: usize,
    scope: Scope,
) -> (StorageBackendResult<Calls>, usize) {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let source = StorageReadControl::new(memory, &original);
    let bytes = Arc::new(serde_json::to_vec(record).unwrap());
    let mut decoder = match ImageDecoder::new(memory, &invoking, move |mut reader| async move {
        let mut calls = Vec::new();
        catalog
            .visit(&bytes, &source, &mut reader.input(), &mut |name, site| {
                calls.push((name.to_owned(), site));
                Ok(())
            })
            .await?;
        Ok::<_, StorageBackendError>(calls)
    }) {
        Ok(decoder) => decoder,
        Err(error) => return (Err(error.into()), 0),
    };
    let mut polls = 0;
    let mut held = None;
    let result = loop {
        polls += 1;
        if polls == 12 {
            match scope {
                Scope::Cancel => invoking.cancel(),
                Scope::Exhaust => held = Some(memory.reserve(memory.available()).unwrap()),
                Scope::Complete => (),
            }
        }
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => (),
            other => break other.map(Option::unwrap),
        }
    };
    drop(decoder);
    drop(held);
    (result, polls)
}

fn trigger() -> StoredTrigger {
    let Statement::CreateTrigger(definition) = uqa_sql::compile("CREATE TRIGGER report BEFORE INSERT ON items FOR EACH ROW WHEN (chosen(NEW.v)) EXECUTE FUNCTION durable_handler()").unwrap().remove(0) else { panic!("trigger") };
    StoredTrigger {
        definition,
        function_object_id: Some([2; 16]),
        enabled: EventEnableMode::Always,
        object_id: None,
        constraint_name: None,
        catalog_oid: None,
        constraint_catalog_oid: None,
    }
}

fn rule() -> StoredRule {
    let Statement::CreateRule(definition) = uqa_sql::compile("CREATE RULE report AS ON INSERT TO items WHERE legacy_condition(NEW.v) DO ALSO INSERT INTO audit VALUES (action_input(NEW.v))").unwrap().remove(0) else { panic!("rule") };
    let mut plan = ExpressionPlan::lower(definition.condition.as_ref().unwrap().clone());
    let ScalarExpr::Func { name, binding, .. } = &mut plan.scalar else {
        panic!("predicate")
    };
    *binding = Some(FunctionBinding::builtin_by_name(name, None));
    let binding = RuleConditionBinding::for_event(&["v".into()], definition.event);
    StoredRule {
        definition,
        enabled: EventEnableMode::Origin,
        condition_plan: Some(plan),
        condition_binding: Some(binding),
        dependencies: None,
        catalog_oid: None,
    }
}

#[test]
fn physical_event_records_preserve_original_selections_and_legacy_positional_fields() {
    use StoredFunctionSite::{Expression, ScalarOnly};
    let memory = MemoryBudget::new(8 << 20);
    let event = trigger();
    let record = json!({"ignored":{"Func":{"name":"data"}},"triggers":[&event]});
    for record in [record, json!([[[&event.definition]]])] {
        assert_eq!(
            read(Catalog::Triggers, &record, &memory, 1, Scope::Complete)
                .0
                .unwrap(),
            [("chosen".into(), ScalarOnly)]
        );
        assert_eq!(memory.used(), 0);
    }
    let event = rule();
    let bound = event.condition_plan.as_ref().unwrap();
    let mut positional = serde_json::to_value(&bound.scalar).unwrap();
    let fields = positional["Func"].as_object().unwrap();
    let ScalarExpr::Func { order_syntax, .. } = &bound.scalar else {
        unreachable!()
    };
    let args = json!([
        fields["name"],
        fields["binding"],
        fields["args"],
        false,
        [],
        order_syntax,
        null
    ]);
    positional["Func"] = args;
    let positional = json!([positional, []]);
    let decoded: ExpressionPlan =
        serde_json::from_slice(&serde_json::to_vec(&positional).unwrap()).unwrap();
    assert_eq!(decoded.scalar, bound.scalar);
    for record in [
        json!({"rules":[&event],"format_version":4}),
        json!([
            4,
            [[
                &event.definition,
                "Origin",
                positional,
                &event.condition_binding
            ]]
        ]),
    ] {
        assert_eq!(
            read(Catalog::Rules, &record, &memory, 1, Scope::Complete)
                .0
                .unwrap(),
            [("action_input".into(), Expression)]
        );
        assert_eq!(memory.used(), 0);
    }
    let legacy = json!({"rules":[{"definition": &event.definition}]});
    assert_eq!(
        read(Catalog::Rules, &legacy, &memory, 1, Scope::Complete)
            .0
            .unwrap(),
        [
            ("legacy_condition".into(), Expression),
            ("action_input".into(), Expression)
        ]
    );
    assert!(read(
        Catalog::Rules,
        &json!({"format_version":u32::MAX}),
        &memory,
        32,
        Scope::Complete
    )
    .0
    .unwrap_err()
    .to_string()
    .contains("newer than supported format"));
    assert!(
        read(Catalog::Triggers, &json!({}), &memory, 32, Scope::Complete)
            .0
            .unwrap()
            .is_empty()
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn physical_event_records_bound_late_calls_cancellation_and_allocation() {
    let memory = MemoryBudget::new(8 << 20);
    let tail = trigger();
    let mut empty = tail.clone();
    empty.definition.when = None;
    let mut entries = vec![json!(empty); 128];
    entries.push(json!(tail));
    let record = json!({"triggers":entries});
    let (calls, single) = read(Catalog::Triggers, &record, &memory, 1, Scope::Complete);
    assert_eq!(
        calls.unwrap(),
        [("chosen".into(), StoredFunctionSite::ScalarOnly)]
    );
    assert_eq!(memory.used(), 0);
    let (calls, grouped) = read(Catalog::Triggers, &record, &memory, 32, Scope::Complete);
    assert_eq!(
        calls.unwrap(),
        [("chosen".into(), StoredFunctionSite::ScalarOnly)]
    );
    assert!(single > grouped && single > 4096);
    assert_eq!(memory.used(), 0);
    let (stopped, polls) = read(Catalog::Triggers, &record, &memory, 1, Scope::Cancel);
    assert!(matches!(stopped, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(polls, 12);
    let (denied, polls) = read(Catalog::Triggers, &record, &memory, 1, Scope::Exhaust);
    assert!(matches!(denied, Err(StorageBackendError::Memory(_))));
    assert!(polls >= 12);
    assert_eq!(memory.used(), 0);
}
