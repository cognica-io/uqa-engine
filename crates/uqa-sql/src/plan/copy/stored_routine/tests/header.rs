//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::RoutineAclEntry;
use uqa_core::catalog_role::RoleIdentity;

fn declaration() -> CreateFunction {
    routine(1, true, "original default").def.clone()
}

fn grant(i: u32) -> RoutineAclEntry {
    let role = RoleIdentity {
        oid: i64::from(i) + 100,
        object_id: u128::from(i + 1).to_le_bytes(),
    };
    RoutineAclEntry {
        role: Some(role),
        grantor: RoleIdentity::BOOTSTRAP,
        grant_option: i.is_multiple_of(2),
    }
}

fn measure(d: &CreateFunction) -> (usize, usize) {
    let acl = d.execute_acl.as_ref().map_or(0, Vec::len);
    let bytes = d
        .creation_search_path
        .iter()
        .map(String::len)
        .sum::<usize>()
        + d.config
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>();
    (d.creation_search_path.len() + d.config.len() + acl, bytes)
}

#[test]
fn stored_routine_catalog_lists_bound_each_entry_and_text_chunk_before_binding() {
    let mut definition = declaration();
    let wide = format!("{}{}", "s".repeat(4095), "𐐷자료".repeat(16384));
    definition.creation_search_path = (0..64).map(|i| format!("schema{i}")).collect();
    definition.creation_search_path.push(wide.clone());
    definition.config = vec![(wide.clone(), String::new()), ("my.setting".into(), wide)];
    definition.execute_acl = Some((0..4096).map(grant).collect());
    let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
    let memory = MemoryBudget::new(4 << 20);
    let original = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &original);
    let mut input = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    let total = measure(&source.def);
    let mut previous = (0, 0);
    let mut windows = 0;
    loop {
        let current = measure(input.declaration());
        assert!(current.0 - previous.0 <= 8);
        assert!(current.1 - previous.1 <= 8 * 4096);
        assert!(input.declaration().params.is_empty());
        windows += 1;
        if current == total {
            break;
        }
        assert_ne!(previous, current, "header stopped advancing");
        previous = current;
        assert!(!input.copy_next_field(&control).unwrap());
    }
    assert!(windows >= 512);
    while !input.copy_next_field(&control).unwrap() {}
    let ordinary_memory = MemoryBudget::new(4 << 20);
    let ordinary = ProductionControl::new(&ordinary_memory, &original, &original);
    let (expected, held) = capture_stored_with_footprint(&source.def, &ordinary).unwrap();
    let (result, children) = input.into_definition(&control).unwrap();
    assert_eq!(result.creation_search_path, source.def.creation_search_path);
    assert_eq!(result.config, source.def.config);
    assert_eq!(result.execute_acl, source.def.execute_acl);
    assert_eq!(
        serde_json::to_value(&*result).unwrap(),
        serde_json::to_value(&*expected).unwrap()
    );
    assert_eq!(
        (result.reserved_bytes(), children),
        (expected.reserved_bytes(), held)
    );
    assert_eq!(memory.used(), result.reserved_bytes() + children);
    drop((result, expected));
    assert_eq!(memory.used(), 0);
    assert_eq!(ordinary_memory.used(), 0);
}

#[test]
fn stored_routine_catalog_lists_release_denied_cancelled_and_abandoned_headers() {
    let mut definition = declaration();
    definition.config = (0..24)
        .map(|i| (format!("my.setting{i}"), "small".into()))
        .collect();
    definition
        .config
        .push(("my.large".into(), "𐐷".repeat(65536)));
    definition.execute_acl = Some(Vec::new());
    let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
    let memory = MemoryBudget::new(64 << 10);
    for cancellation in [None, Some(true), Some(false)] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut input = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        assert!(input.declaration().config.len() < source.def.config.len());
        assert!(!input.copy_next_field(&control).unwrap());
        if let Some(session) = cancellation {
            if session {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let failure = loop {
            match input.copy_next_field(&control) {
                Ok(false) => (),
                Ok(true) => panic!("unfinished catalog reached binding"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            failure.sqlstate(),
            Some(if cancellation.is_some() {
                "57014"
            } else {
                "53200"
            })
        );
        assert!(input.declaration().params.is_empty());
        assert!(input.into_definition(&control).is_err());
        assert_eq!(memory.used(), 0);
    }
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut abandoned = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    assert!(!abandoned.copy_next_field(&control).unwrap());
    drop(abandoned);
    assert_eq!(memory.used(), 0);
    assert_eq!(source.def.config.last().unwrap().1.len(), 1 << 18);

    let mut definition = declaration();
    definition.creation_search_path = (0..64).map(|i| format!("schema{i}")).collect();
    definition.execute_acl = Some((0..4096).map(grant).collect());
    let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
    let mut input = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    let error = loop {
        match input.copy_next_field(&control) {
            Ok(false) => (),
            Ok(true) => panic!("unadmitted ACL was published"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(
        input.declaration().creation_search_path,
        source.def.creation_search_path
    );
    assert!(input.declaration().execute_acl.as_ref().unwrap().is_empty());
    drop(input);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_routine_catalog_lists_preserve_the_ordinary_peak_and_optional_acl() {
    let token = CancellationToken::new();
    for acl in [None, Some(Vec::new()), Some(vec![grant(17)])] {
        let mut definition = routine(0, false, "").def.clone();
        definition.creation_search_path = vec!["public".into()];
        definition.config = vec![("my.setting".into(), "𐐷".repeat(32768))];
        definition.execute_acl = acl;
        let source = SQLUserFunction::new(definition, RoutineBody::Source).shared();
        let ordinary = MemoryBudget::new(1 << 20);
        let control = ProductionControl::new(&ordinary, &token, &token);
        let (reference, _) = capture_stored_with_footprint(&source.def, &control).unwrap();
        let peak = ordinary.peak();
        let memory = MemoryBudget::new(peak);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut input = StoredRoutineDefinition::new(source, &control).unwrap();
        while !input.copy_next_field(&control).unwrap() {}
        let (result, _) = input.into_definition(&control).unwrap();
        assert_eq!(result.execute_acl, reference.execute_acl);
        assert_eq!(result.config, reference.config);
        assert_eq!(result.reserved_bytes(), reference.reserved_bytes());
        assert_eq!(memory.peak(), peak);
        drop((result, reference));
        assert_eq!(memory.used(), 0);
        assert_eq!(ordinary.used(), 0);
    }
}
