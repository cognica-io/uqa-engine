//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::routines::{
    lifecycle::{restoration::RoutineRestorePlaceholders, RoutineRegistry},
    routine_signature_types, CompiledFunctionBody, RoutineBody,
};

fn finish(
    input: &mut RoutineCatalogPlaceholderInput,
    steps: usize,
) -> StorageBackendResult<(RoutineRestorePlaceholders, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("routine placeholder input stopped progressing")
}

#[test]
fn stored_routine_placeholder_input_preserves_complete_canonical_validation_and_deferred_bodies() {
    let mut bound = definition("f", 3, "bigint");
    bound["body"] = serde_json::to_value(FunctionBody::Statements(
        uqa_sql::compile("SELECT 42").unwrap(),
    ))
    .unwrap();
    let text = document(json!({
        "f": [definition("f", 1, "domain_alias"), definition("public.f", 2, "integer"), bound],
        "public.empty": []
    }));
    let expected = original(&text, &Names::new()).unwrap();
    for steps in [1, 31, 1024] {
        let native = MemoryBudget::new(16 << 20);
        let control = StorageReadControl::with_limit(16 << 20);
        let previous = RoutineRegistry::new();
        let mut input = RoutineCatalogPlaceholderInput::new(
            catalog(&text),
            roles(),
            Names::new(),
            previous.clone(),
            LIMITS,
            &native,
            &control,
        )
        .unwrap();
        let (output, polls) = finish(&mut input, steps).unwrap();
        assert!(polls > 1);
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
        let (registry, pending) = output.into_parts();
        assert!(previous.is_empty());
        let actual = serde_json::to_value(
            pending
                .groups()
                .map(|group| {
                    (
                        group[0].def.name.as_str(),
                        group.iter().map(|routine| &routine.def).collect::<Vec<_>>(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(registry.len(), 1);
        let overloads = &registry["public.f"];
        let signatures = overloads
            .iter()
            .map(|routine| routine_signature_types(&routine.def))
            .collect::<Vec<_>>();
        assert!(signatures.windows(2).all(|pair| pair[0] < pair[1]));
        for routine in overloads {
            let RoutineBody::Bound(body) = &routine.body else {
                panic!("bound placeholder")
            };
            assert!(matches!(&***body, CompiledFunctionBody::SQL(plans) if plans.is_empty()));
            assert!(routine.is_retained_by(&native));
        }
        drop(pending);
        assert!(native.used() > 0);
        drop(registry);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn stored_routine_placeholder_input_rejects_original_errors_before_publishing_any_namespace() {
    let cases = [
        document(json!({"f": [definition("f", 1, "integer"), definition("f", 2, "int")]})),
        document(json!({"f": [definition("other", 1, "integer")]})),
        document(json!({"missing.f": [definition("missing.f", 1, "integer")]})),
        json!({"routine_catalog_format": 2, "definitions": {}}).to_string(),
    ];
    for text in cases {
        let expected = original(&text, &Names::new()).unwrap_err();
        let native = MemoryBudget::new(1 << 20);
        let control = StorageReadControl::with_limit(1 << 20);
        let previous = RoutineRegistry::new();
        let mut input = RoutineCatalogPlaceholderInput::new(
            catalog(&text),
            roles(),
            Names::new(),
            previous.clone(),
            LIMITS,
            &native,
            &control,
        )
        .unwrap();
        let error = finish(&mut input, 1).err().unwrap();
        assert_eq!(error.to_string(), expected.to_string());
        drop(error);
        assert!(input.advance(1).is_err());
        drop(input);
        assert!(previous.is_empty());
        assert_eq!((native.used(), control.memory().used()), (0, 0));
    }
}
