//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn check<'a>(declarations: impl IntoIterator<Item = NativeNameInput<'a>>) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(1 << 20);
    let result = validate(declarations.into_iter(), &memory, &CancellationToken::new());
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_name_claims_separate_plain_relations_from_row_and_scalar_types() {
    let name = RelationIdentity::new("public", "same");
    check([
        NativeNameInput::Relation(&name),
        NativeNameInput::TypeWithArray(&name, None),
    ])
    .unwrap();
    assert!(check([
        NativeNameInput::Relation(&name),
        NativeNameInput::Relation(&name)
    ])
    .is_err());
    assert!(check([
        NativeNameInput::RowRelation(&name, None),
        NativeNameInput::Type(&name)
    ])
    .is_err());
    assert!(check([
        NativeNameInput::Type(&name),
        NativeNameInput::TypeWithArray(&name, None)
    ])
    .is_err());
    let elsewhere = RelationIdentity::new("other", "same");
    check([
        NativeNameInput::RowRelation(&name, None),
        NativeNameInput::RowRelation(&elsewhere, None),
    ])
    .unwrap();
    let dotted_schema = RelationIdentity::new("a.b", "c");
    let dotted_name = RelationIdentity::new("a", "b.c");
    check([
        NativeNameInput::RowRelation(&dotted_schema, None),
        NativeNameInput::RowRelation(&dotted_name, None),
    ])
    .unwrap();
}

#[test]
fn native_name_claims_preserve_displaced_arrays_and_legacy_utf8_clipping() {
    let owner = RelationIdentity::new("public", "items");
    let displaced = RelationIdentity::new("public", "_items");
    assert!(check([
        NativeNameInput::RowRelation(&owner, None),
        NativeNameInput::Type(&displaced)
    ])
    .is_err());
    check([
        NativeNameInput::RowRelation(&owner, Some("_items_1")),
        NativeNameInput::Type(&displaced),
    ])
    .unwrap();
    let unicode = RelationIdentity::new("public", "é".repeat(32));
    let array = RelationIdentity::new("public", format!("_{}", "é".repeat(31)));
    assert!(check([
        NativeNameInput::TypeWithArray(&unicode, None),
        NativeNameInput::Type(&array)
    ])
    .is_err());
    let memory = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let name = crate::catalog::array_type_names::default_array_type_name_with_control(
        &unicode.name,
        &control,
    )
    .unwrap();
    assert_eq!(*name, array.name);
    assert_eq!(memory.used(), name.capacity());
    drop(name);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_name_claims_reject_fixed_relation_scalar_array_and_pseudo_type_names() {
    for (schema, name) in [
        ("pg_catalog", "pg_class"),
        ("information_schema", "tables"),
        ("pg_catalog", "int4"),
        ("pg_catalog", "_int4"),
        ("pg_catalog", "trigger"),
        ("pg_catalog", "anyelement"),
        ("information_schema", "character_data"),
        ("information_schema", "_character_data"),
        ("ag_catalog", "agtype"),
        ("ag_catalog", "_agtype"),
    ] {
        let identity = RelationIdentity::new(schema, name);
        assert!(
            check([NativeNameInput::Type(&identity)]).is_err(),
            "{identity:?}"
        );
    }
    let relation = RelationIdentity::new("pg_catalog", "pg_class");
    assert!(check([NativeNameInput::Relation(&relation)]).is_err());
    let identity = RelationIdentity::new("public", "int4");
    check([NativeNameInput::TypeWithArray(&identity, None)]).unwrap();
}

#[test]
fn native_name_claims_release_all_name_and_map_owners_on_denial_or_cancellation() {
    let names = [
        RelationIdentity::new("public", "first"),
        RelationIdentity::new("public", "second"),
    ];
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(validate(
        names
            .iter()
            .map(|name| NativeNameInput::RowRelation(name, None)),
        &empty,
        &token
    )
    .is_err());
    assert_eq!(empty.used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    assert!(validate(
        names.iter().enumerate().map(|(position, name)| {
            if position == 1 {
                token.cancel();
            }
            NativeNameInput::RowRelation(name, None)
        }),
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
    check(
        names
            .iter()
            .map(|name| NativeNameInput::RowRelation(name, None)),
    )
    .unwrap();
}
