//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn default_capture(source: SQLRoutine, memory: &MemoryBudget) -> (usize, usize) {
    let token = CancellationToken::new();
    let control = ProductionControl::new(memory, &token, &token);
    let reference_memory = MemoryBudget::new(16 << 20);
    let reference = ProductionControl::new(&reference_memory, &token, &token);
    let (expected, children) = capture_stored_with_footprint(&source.def, &reference).unwrap();
    let mut captured = StoredRoutineDefinition::new(source, &control).unwrap();
    let mut steps = 1;
    while !captured.copy_next_field(&control).unwrap() {
        assert!(captured.declaration().params.is_empty());
        steps += 1;
    }
    let (actual, held) = captured.into_definition(&control).unwrap();
    assert_eq!(actual.params[0].default, expected.params[0].default);
    assert_eq!(
        actual.params[0].default_type,
        expected.params[0].default_type
    );
    assert_eq!(
        (actual.reserved_bytes(), held),
        (expected.reserved_bytes(), children)
    );
    let retained = memory.used();
    drop((actual, expected));
    assert_eq!((memory.used(), reference_memory.used()), (0, 0));
    (steps, retained)
}

#[test]
fn stored_parameter_default_copies_wide_and_nested_native_expressions_across_windows() {
    let memory = MemoryBudget::new(16 << 20);
    let small = MemoryBudget::new(16 << 10);
    let array = |width| {
        let items = std::iter::repeat_n("1", width)
            .collect::<Vec<_>>()
            .join(",");
        deferred(
            "SELECT cardinality(v)",
            &format!("v integer[] DEFAULT ARRAY[{items}]"),
        )
    };
    let short = default_capture(array(2), &small);
    let wide = default_capture(array(4096), &memory);
    assert!(wide.0 >= short.0 + 128, "short {short:?}, wide {wide:?}");
    assert!(wide.1 >= short.1 + 4094 * size_of::<crate::ast::Expr>());
    let expression = |depth| {
        let body = format!("{}1{}", "abs(".repeat(depth), ")".repeat(depth));
        deferred("SELECT v", &format!("v integer DEFAULT {body}"))
    };
    let shallow = default_capture(expression(1), &small);
    let nested = default_capture(expression(32), &memory);
    assert!(
        nested.0 > shallow.0,
        "shallow {shallow:?}, nested {nested:?}"
    );
    eprintln!(
        "default fields two {short:?}, 4096 {wide:?}, one nested call {shallow:?}, 32 {nested:?}, small peak {}",
        small.peak()
    );
}

#[test]
fn stored_parameter_default_abandonment_denial_and_cancellation_release_private_fields() {
    let items = std::iter::repeat_n("'early'", 128)
        .collect::<Vec<_>>()
        .join(",");
    let source = deferred(
        "SELECT 1",
        &format!(
            "v text[] DEFAULT ARRAY[{items},'{}']",
            "z".repeat(512 << 10)
        ),
    );
    for stop in 0..4 {
        let memory = MemoryBudget::new(256 << 10);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut captured = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        let initial = memory.used();
        assert!(!captured.copy_next_field(&control).unwrap());
        assert!(captured.declaration().params.is_empty());
        assert!(memory.used() > initial);
        match stop {
            0 => {}
            1 => original.cancel(),
            2 => invoking.cancel(),
            _ => {
                drop(captured);
                assert_eq!(memory.used(), 0);
                continue;
            }
        }
        let failure = loop {
            match captured.copy_next_field(&control) {
                Ok(false) => assert!(captured.declaration().params.is_empty()),
                Ok(true) => panic!("oversized final value was published"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            failure.sqlstate(),
            Some(if stop == 0 { "53200" } else { "57014" })
        );
        assert!(captured.declaration().params.is_empty());
        assert_eq!(memory.used(), initial);
        assert!(captured.into_definition(&control).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn edited_parameter_publishes_its_current_value_and_receipt_only_after_reprojection() {
    use crate::catalog::work::inline;

    let width = 4096;
    let values = std::iter::repeat_n("1", width)
        .collect::<Vec<_>>()
        .join(",");
    let source = deferred(
        "SELECT cardinality(v)",
        &format!("v integer[] DEFAULT ARRAY[{values}]"),
    );
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut definition = StoredRoutineDefinition::new(source, &control).unwrap();
    while !definition.copy_next_field(&control).unwrap() {}
    definition
        .edit_parameter(0, &control, |parameter| {
            parameter.name = "previous_edit".into();
            Ok(true)
        })
        .unwrap();
    let before = budget.used();
    assert!(inline(definition.parameter_edit_from(1, &control, &mut ()))
        .unwrap()
        .is_none());
    for finish in [false, true] {
        let mut work = Checkpoints::default();
        let mut edit = inline(definition.parameter_edit_from(0, &control, &mut work))
            .unwrap()
            .unwrap();
        assert!(work.0 >= width, "wide scratch used {} steps", work.0);
        assert_eq!(edit.value_mut().name, "previous_edit");
        edit.value_mut().name = "private_edit".into();
        let pressure = budget.reserve(budget.available()).unwrap();
        let result = inline(edit.finish_from(finish, &control, &mut work));
        if finish {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            assert!(!result.unwrap());
        }
        drop(pressure);
        assert_eq!(definition.declaration().params[0].name, "previous_edit");
        assert_eq!(budget.used(), before);
    }
    let stopped = CancellationToken::new();
    let cancellable = ProductionControl::new(&budget, &original, &stopped);
    let mut abandoned = inline(definition.parameter_edit_from(0, &cancellable, &mut ()))
        .unwrap()
        .unwrap();
    abandoned.value_mut().name = "cancelled_edit".into();
    stopped.cancel();
    assert_eq!(
        inline(abandoned.finish_from(true, &cancellable, &mut ()))
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), before);

    let mut edit = inline(definition.parameter_edit_from(0, &control, &mut ()))
        .unwrap()
        .unwrap();
    edit.value_mut().name = "published_edit".into();
    let mut work = Checkpoints::default();
    assert!(inline(edit.finish_from(true, &control, &mut work)).unwrap());
    assert!(work.0 >= width, "wide publication used {} steps", work.0);
    let reference = MemoryBudget::new(16 << 20);
    let (expected, child_bytes) = capture_stored_with_footprint(
        definition.declaration(),
        &ProductionControl::new(&reference, &original, &invoking),
    )
    .unwrap();
    let (value, children) = definition.into_definition(&control).unwrap();
    assert_eq!(value.params[0].name, "published_edit");
    assert_eq!(value.params[0].default, expected.params[0].default);
    assert_eq!(
        (value.reserved_bytes(), children),
        (expected.reserved_bytes(), child_bytes)
    );
    drop((value, expected));
    assert_eq!((budget.used(), reference.used()), (0, 0));
}
