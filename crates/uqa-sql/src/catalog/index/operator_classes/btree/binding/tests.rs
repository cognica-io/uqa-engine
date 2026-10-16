//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, EnumTypeReference};
use crate::catalog::index::operator_classes::builtin_btree_operator_classes;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn declared_type(name: &str) -> ColumnType {
    let name = name.strip_prefix("public.").unwrap_or(name);
    match name {
        "integer_domain" | "array_domain" => ColumnType::Domain {
            schema: "public".into(),
            name: name.into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(if name == "integer_domain" {
                ColumnType::Integer
            } else {
                ColumnType::Array(Box::new(ColumnType::Oid))
            }),
        },
        "bound_record" => ColumnType::Composite(CompositeTypeReference {
            schema: "public".into(),
            name: name.into(),
            oid: 50_003,
            array_oid: 50_004,
            relation_oid: 50_005,
        }),
        "bound_enum" => ColumnType::Enum(EnumTypeReference {
            schema: "public".into(),
            name: name.into(),
            oid: 50_006,
            array_oid: 50_007,
        }),
        _ => ColumnType::from_sql_name(name).unwrap(),
    }
}

fn class(name: &str) -> &'static BuiltinBTreeOperatorClass {
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
    builtin_btree_operator_classes()
        .iter()
        .find(|class| class.name == name)
        .unwrap()
}

#[test]
fn selected_btree_inputs_match_postgresql_range_declarations() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let _scope = crate::ast::TypeDisplayScope::enter(&["public".into()]);
    let mut accepted = 0;
    let mut rejected = 0;
    for case in reference["cases"].as_array().unwrap() {
        let selected_name = case["operator_class"].as_str().unwrap();
        let selected = class(selected_name);
        let ty = declared_type(case["subtype"].as_str().unwrap());
        let bound = selected.bind_input(&ty, selected_name, &control);
        if case["accepted"].as_bool().unwrap() {
            let bound = bound.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert!(std::ptr::eq(bound.declared_type(), &raw const ty));
            assert!(std::ptr::eq(bound.class(), selected));
            accepted += 1;
        } else {
            let error = bound.unwrap_err();
            assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{case}");
            assert_eq!(error.to_string(), case["message"].as_str().unwrap());
            assert_eq!(error.detail().unwrap_or_default(), case["detail"]);
            assert_eq!(error.hint().unwrap_or_default(), case["hint"]);
            rejected += 1;
        }
        assert_eq!(memory.used(), 0);
    }
    assert_eq!((accepted, rejected), (23, 13));
}

#[test]
fn selected_btree_binding_preserves_modifiers_and_owns_only_cast_lookup_scratch() {
    let memory = MemoryBudget::new(4096);
    let local = CancellationToken::new();
    let external = CancellationToken::new();
    let control = ProductionControl::new(&memory, &local, &external);
    for (ty, selected) in [
        (ColumnType::Varchar(Some(19)), "bpchar_ops"),
        (ColumnType::Character(7), "bpchar_pattern_ops"),
        (ColumnType::Bit(Some(13)), "varbit_ops"),
        (ColumnType::VarBit(Some(17)), "bit_ops"),
        (declared_type("integer_domain"), "oid_ops"),
    ] {
        let bound = class(selected).bind_input(&ty, selected, &control).unwrap();
        assert!(std::ptr::eq(bound.declared_type(), &raw const ty));
        assert_eq!(memory.used(), 0);
    }
    let empty = MemoryBudget::new(0);
    let limited = ProductionControl::new(&empty, &local, &external);
    class("int4_ops")
        .bind_input(&ColumnType::Integer, "int4_ops", &limited)
        .unwrap();
    class("array_ops")
        .bind_input(&ColumnType::OidVector, "array_ops", &limited)
        .unwrap();
    assert_eq!(empty.peak(), 0);
    let error = class("oid_ops")
        .bind_input(&ColumnType::Integer, "oid_ops", &limited)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(empty.used(), 0);
    for cancelled in [&local, &external] {
        cancelled.cancel();
        let error = class("int4_ops")
            .bind_input(&ColumnType::Integer, "int4_ops", &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        cancelled.reset();
    }
    assert_eq!(memory.used(), 0);
}
