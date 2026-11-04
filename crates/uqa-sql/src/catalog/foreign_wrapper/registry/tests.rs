//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{definition_capture::foreign, foreign_wrapper::*, roles::RoleIdentity};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn wrapper(index: u32) -> ForeignWrapperDefinition {
    let callback = ForeignWrapperFunction {
        oid: 40000,
        binding: crate::ast::FunctionBinding {
            name: "Exact.Schema.callback".into(),
            object_id: Some([9; 16]),
            argument_types: vec!["text[]".into(), "oid".into()],
            builtin: false,
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        },
    };
    ForeignWrapperDefinition {
        name: format!("wrapper_{index:04}"),
        identity: ForeignWrapperReference {
            oid: 20000 + index,
            object_id: u128::from(index + 1).to_be_bytes(),
        },
        owner: RoleIdentity::BOOTSTRAP,
        handler: match index % 3 {
            0 => ForeignWrapperHandler::Native(NativeForeignWrapper::Memory),
            1 => ForeignWrapperHandler::None,
            _ => ForeignWrapperHandler::Function(callback.clone()),
        },
        validator: Some(callback),
        options: vec![
            ("z".into(), "first\nvalue".into()),
            ("a".into(), "second 한글".into()),
        ],
    }
}

#[test]
fn wrapper_roots_capture_without_allocations_and_preserve_original_encodings() {
    for count in [1, 64, 4096] {
        let mut wrappers = ForeignWrappers::try_from_iter((0..count).map(|index| {
            let value = wrapper(index);
            (value.name.clone(), value)
        }))
        .unwrap();
        for (index, (_, value)) in wrappers.iter().enumerate() {
            assert_eq!(value, &wrapper(u32::try_from(index).unwrap()));
        }
        let original = wrappers.clone();
        let budget = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut saved = None;
        let allocations =
            allocation_counter::measure(|| saved = Some(wrappers.capture(&control).unwrap()));
        assert_eq!(allocations.count_total, 0);
        let required = budget.used();
        let exact = MemoryBudget::new(required);
        let independent = wrappers
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), required);
        let legacy = foreign::wrappers(&wrappers, &control).unwrap();
        assert!(saved
            .as_ref()
            .unwrap()
            .iter()
            .eq(legacy.iter().map(|(k, v)| (k, v))));
        assert_eq!(
            serde_json::to_vec(&**saved.as_ref().unwrap()).unwrap(),
            serde_json::to_vec(&*legacy).unwrap()
        );
        let map = wrappers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let encoded = serde_json::to_vec(&map).unwrap();
        assert_eq!(serde_json::to_vec(&wrappers).unwrap(), encoded);
        assert_eq!(
            serde_json::from_slice::<ForeignWrappers>(&encoded).unwrap(),
            wrappers
        );
        wrappers.clear();
        drop((wrappers, legacy));
        assert!(saved.as_ref().unwrap().iter().eq(original.iter()));
        assert!(independent.iter().eq(original.iter()));
        drop((saved, independent));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn selected_wrapper_publication_shares_unrelated_options_and_callback_payloads() {
    for count in [1, 32, 512] {
        let mut wrappers = ForeignWrappers::try_from_iter((0..count).map(|index| {
            let value = wrapper(index);
            (value.name.clone(), value)
        }))
        .unwrap();
        let mut large = wrapper(9000);
        large.options[0].1 = "large".repeat(1 << 15);
        large.validator.as_mut().unwrap().binding.name = "callback".repeat(1 << 14);
        wrappers.insert(large.name.clone(), large).unwrap();
        let original = wrappers.clone();
        let allocations = allocation_counter::measure(|| {
            wrappers
                .edit("wrapper_0000", |value| {
                    value.handler = ForeignWrapperHandler::None;
                    value.options.reverse();
                    Ok(())
                })
                .unwrap()
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        for name in original.keys().filter(|name| *name != "wrapper_0000") {
            assert!(wrappers.shares_definition(name, &original));
        }
        assert_eq!(original["wrapper_0000"], wrapper(0));
        assert_eq!(wrappers["wrapper_0000"].options[0].0, "a");
        assert_eq!(
            wrappers["wrapper_0000"].handler,
            ForeignWrapperHandler::None
        );
    }
}

#[test]
fn denied_wrapper_publication_and_capture_preserve_complete_original_references() {
    let native = MemoryBudget::new(65536);
    let mut wrappers = ForeignWrappers::with_budget(&native);
    wrappers.insert("wrapper_0000".into(), wrapper(0)).unwrap();
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let saved = wrappers.capture(&control).unwrap();
    let small = MemoryBudget::new(budget.used() - 1);
    assert!(wrappers
        .capture(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    let blocked = native.reserve(native.available()).unwrap();
    assert!(wrappers
        .edit("wrapper_0000", |value| {
            value.validator = None;
            Ok(())
        })
        .is_err());
    assert_eq!(wrappers["wrapper_0000"], wrapper(0));
    assert_eq!(saved.get("wrapper_0000"), Some(&wrapper(0)));
    token.cancel();
    assert!(matches!(
        wrappers.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((blocked, wrappers));
    assert!(native.used() > 0);
    drop(saved);
    assert_eq!(native.used(), 0);
    assert_eq!(budget.used(), 0);
}
