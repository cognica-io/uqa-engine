//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    RoutineAttributeClause, RoutineBodyError, RoutineColumnTypeReference, RoutineConfigAction,
    RoutineTransformType,
};

fn wide_text() -> String {
    format!("{}{}", "n".repeat(4095), "𐐷자료".repeat(8192))
}

fn raw_definition() -> CreateFunction {
    let mut d = declaration();
    let text = wide_text();
    d.name.clone_from(&text);
    d.return_type_reference = Some(RoutineColumnTypeReference::new(
        Some(text.clone()),
        text.clone(),
        text.clone(),
    ));
    d.return_written_type = Some(text.clone());
    d.language.clone_from(&text);
    d.support = Some(text.clone());
    d.config_actions = vec![
        RoutineConfigAction::ResetAll,
        RoutineConfigAction::FromCurrent { name: text.clone() },
        RoutineConfigAction::Reset {
            name: String::new(),
        },
        RoutineConfigAction::Set {
            name: text.clone(),
            value: text.clone(),
        },
    ];
    d.attribute_clauses.clauses = vec![RoutineAttributeClause::Transform; 4096];
    d.attribute_clauses.invalid_parallel = Some(text.clone());
    d.attribute_clauses.transform_types = vec![RoutineTransformType {
        type_name: text.clone(),
        written: text,
    }];
    d.attribute_clauses.body_error = Some(RoutineBodyError::ExtraAsItems);
    d
}

fn fields(d: &CreateFunction) -> [&str; 13] {
    let reference = d.return_type_reference.as_ref().unwrap();
    let current = match d.config_actions.get(1) {
        Some(RoutineConfigAction::FromCurrent { name }) => name.as_str(),
        _ => "",
    };
    let setting = match d.config_actions.get(3) {
        Some(RoutineConfigAction::Set { name, value }) => (name.as_str(), value.as_str()),
        _ => ("", ""),
    };
    let transform = d.attribute_clauses.transform_types.first();
    [
        &d.name,
        reference.schema.as_deref().unwrap(),
        &reference.relation,
        &reference.column,
        d.return_written_type.as_deref().unwrap(),
        &d.language,
        d.support.as_deref().unwrap(),
        current,
        setting.0,
        setting.1,
        d.attribute_clauses.invalid_parallel.as_deref().unwrap(),
        transform.map_or("", |t| t.type_name.as_str()),
        transform.map_or("", |t| t.written.as_str()),
    ]
}

#[test]
fn stored_raw_routine_header_fields_yield_in_declaration_order_and_match_ordinary_capture() {
    let source = SQLUserFunction::new(raw_definition(), RoutineBody::Source).shared();
    let token = CancellationToken::new();
    let ordinary = MemoryBudget::new(8 << 20);
    let (expected, expected_children) = capture_stored_with_footprint(
        &source.def,
        &ProductionControl::new(&ordinary, &token, &token),
    )
    .unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let control = ProductionControl::new(&memory, &token, &token);
    let mut capture = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    let full = fields(&source.def);
    let mut previous = [0; 13];
    let mut clauses = 0;
    let mut windows = 0;
    loop {
        let value = capture.declaration();
        let mut earlier_complete = true;
        for (index, (field, target)) in fields(value).into_iter().zip(full).enumerate() {
            assert!(target.starts_with(field));
            if !earlier_complete {
                assert!(
                    field.is_empty(),
                    "field {index} preceded its source dependency"
                );
            }
            assert!(
                field.len() - previous[index] <= 8 * 4096,
                "field {index} copied in one step"
            );
            previous[index] = field.len();
            earlier_complete &= field.len() == target.len();
        }
        let completed = value.attribute_clauses.clauses.len();
        assert!(completed - clauses <= 8);
        if completed < source.def.attribute_clauses.clauses.len() {
            assert!(value
                .attribute_clauses
                .invalid_parallel
                .as_ref()
                .unwrap()
                .is_empty());
        }
        clauses = completed;
        if earlier_complete {
            break;
        }
        assert!(value.params.is_empty());
        windows += 1;
        assert!(!capture.copy_next_field(&control).unwrap());
    }
    assert!(windows >= 512);
    while !capture.copy_next_field(&control).unwrap() {}
    let (actual, children) = capture.into_definition(&control).unwrap();
    assert_eq!(
        serde_json::to_value(&*actual).unwrap(),
        serde_json::to_value(&*expected).unwrap()
    );
    assert_eq!(
        (actual.reserved_bytes(), children),
        (expected.reserved_bytes(), expected_children)
    );
    assert_eq!(memory.used(), actual.reserved_bytes() + children);
    drop((actual, expected));
    assert_eq!((memory.used(), ordinary.used()), (0, 0));
}

#[test]
fn stored_raw_routine_header_actions_release_partial_and_denied_copies() {
    let mut definition = routine(0, false, "").def.clone();
    let key = "k".repeat(10 << 10);
    definition.config_actions = vec![RoutineConfigAction::ResetAll; 32];
    definition.config_actions.push(RoutineConfigAction::Set {
        name: key.clone(),
        value: wide_text(),
    });
    definition.return_written_type = Some(String::new());
    definition.attribute_clauses.invalid_parallel = Some("later".repeat(100 << 10));
    let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
    for cancelled in [None, Some(true), Some(false)] {
        let memory = MemoryBudget::new(64 << 10);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut capture = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        while capture.declaration().config_actions.len() < 16 {
            assert!(!capture.copy_next_field(&control).unwrap());
        }
        assert_eq!(
            capture.declaration().return_written_type.as_deref(),
            Some("")
        );
        assert_eq!(
            capture
                .declaration()
                .attribute_clauses
                .invalid_parallel
                .as_deref(),
            Some("")
        );
        match cancelled {
            Some(true) => original.cancel(),
            Some(false) => invoking.cancel(),
            None => (),
        }
        let err = loop {
            match capture.copy_next_field(&control) {
                Ok(false) => (),
                Ok(true) => panic!("an incomplete declaration cannot publish"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            err.sqlstate(),
            Some(if cancelled.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        if cancelled.is_none() {
            let ordinary = MemoryBudget::new(64 << 10);
            let expected = capture_stored_with_footprint(
                &source.def,
                &ProductionControl::new(&ordinary, &original, &invoking),
            )
            .err()
            .unwrap();
            assert_eq!(err.to_string(), expected.to_string());
            assert_eq!(ordinary.used(), 0);
            assert!(
                matches!(capture.declaration().config_actions.last(), Some(RoutineConfigAction::Set { name, value }) if name == &key && value.is_empty())
            );
        }
        assert!(capture.declaration().params.is_empty());
        assert!(capture.into_definition(&control).is_err());
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(source.def.config_actions.len(), 33);
}

#[test]
fn raw_routine_header_fits_the_ordinary_allowance_with_optional_names() {
    let token = CancellationToken::new();
    for schema in [None, Some(String::new()), Some("source_schema".into())] {
        let mut definition = routine(0, false, "").def.clone();
        definition.return_type_reference = Some(RoutineColumnTypeReference::new(
            schema,
            "r".into(),
            "c".into(),
        ));
        definition.return_written_type = Some(String::new());
        definition.support = None;
        definition.config_actions = vec![RoutineConfigAction::Reset { name: wide_text() }];
        definition.attribute_clauses.invalid_parallel = Some(String::new());
        definition.attribute_clauses.transform_types = vec![RoutineTransformType {
            type_name: "public.t".into(),
            written: wide_text(),
        }];
        let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
        let ordinary = MemoryBudget::new(2 << 20);
        let (expected, expected_children) = capture_stored_with_footprint(
            &source.def,
            &ProductionControl::new(&ordinary, &token, &token),
        )
        .unwrap();
        let memory = MemoryBudget::new(ordinary.peak());
        let control = ProductionControl::new(&memory, &token, &token);
        let mut input = StoredRoutineDefinition::new(source, &control).unwrap();
        while !input.copy_next_field(&control).unwrap() {}
        let (actual, children) = input.into_definition(&control).unwrap();
        assert_eq!(
            serde_json::to_value(&*actual).unwrap(),
            serde_json::to_value(&*expected).unwrap()
        );
        assert_eq!(
            (actual.reserved_bytes(), children, memory.peak()),
            (
                expected.reserved_bytes(),
                expected_children,
                ordinary.peak()
            )
        );
        drop((actual, expected));
        assert_eq!((memory.used(), ordinary.used()), (0, 0));
    }
}
