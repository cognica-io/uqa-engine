//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::result::format_postgres_text_with_catalog_and_control;
use uqa_core::DatumValue;

mod comparison;

fn reference() -> Reference {
    Reference::from_data(serde_json::from_str(include_str!("ranges/postgresql.json")).unwrap())
}

fn datum(node: &serde_json::Value) -> DatumValue {
    let Field::Node(node) = node_tree::parse(node.as_str().unwrap()).unwrap() else {
        panic!("constant")
    };
    let Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
        panic!("physical value")
    };
    let oid = match node.field("consttype").unwrap() {
        Field::Atom(oid) => oid.parse().unwrap(),
        _ => panic!("constant OID"),
    };
    DatumValue::new(oid, 0, bytes.clone())
}

#[test]
fn declared_range_output_preserves_postgresql_identity_and_typed_endpoints() {
    let mut reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        let ty = reference.range_type(name);
        let oid = crate::catalog::type_metadata::pg_type_oid(&ty) as u32;
        let value = Value::Datum(DatumValue::new(oid, 0, reference.bytes(name)));
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let rendered =
            format_postgres_text_with_catalog_and_control(&value, &ty, Some(&reference), &control)
                .unwrap();
        assert_eq!(
            *rendered,
            case(&reference.data, &format!("{name}_bounds"))["results"][0]["rows"][0][0]
                .as_str()
                .unwrap(),
            "{name}"
        );
        assert_eq!(rendered.reserved_bytes(), memory.used());
        drop(rendered);
        assert_eq!(memory.used(), 0);
        let error =
            format_postgres_text_with_catalog_and_control(&value, &ty, None, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        assert_eq!(
            error.to_string(),
            format!("type with OID {oid} does not exist")
        );
        assert_eq!(memory.used(), 0);
    }
    let ty = reference.range_type("enum_window");
    let value = Value::Datum(DatumValue::new(
        integer(&reference.metadata("enum_window")[1]),
        0,
        reference.bytes("enum_window"),
    ));
    Arc::make_mut(&mut reference.labels).labels[0].label = "lower".into();
    assert_eq!(
        crate::result::format_postgres_text_with_catalog(&value, &ty, Some(&reference)).unwrap(),
        case(&reference.data, "enum_after_rename")["results"][0]["rows"][0][0]
            .as_str()
            .unwrap()
    );
}

#[test]
fn declared_range_output_rejects_truncation_exhaustion_and_both_cancellations() {
    let reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        let ty = reference.range_type(name);
        let oid = crate::catalog::type_metadata::pg_type_oid(&ty) as u32;
        let bytes = reference.bytes(name);
        for end in 0..bytes.len() {
            let value = Value::Datum(DatumValue::new(oid, 0, bytes[..end].to_vec()));
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let result = format_postgres_text_with_catalog_and_control(
                &value,
                &ty,
                Some(&reference),
                &ProductionControl::new(&memory, &token, &token),
            );
            assert!(result.is_err(), "{name} truncated to {end}");
            drop(result);
            assert_eq!(memory.used(), 0);
        }
        for (budget, cancel_original, cancel_invoking, sqlstate) in [
            (0, false, false, "53200"),
            (1 << 20, true, false, "57014"),
            (1 << 20, false, true, "57014"),
        ] {
            let memory = MemoryBudget::new(budget);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            }
            if cancel_invoking {
                invoking.cancel();
            }
            let value = Value::Datum(DatumValue::new(oid, 0, bytes.clone()));
            let error = format_postgres_text_with_catalog_and_control(
                &value,
                &ty,
                Some(&reference),
                &ProductionControl::new(&memory, &original, &invoking),
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some(sqlstate), "{name}");
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn declared_multirange_array_and_empty_output_match_postgresql_owned_payloads() {
    let reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        for (source, expected) in [
            ("multi_catalog", "multi_output"),
            ("empty_catalog", "empty"),
        ] {
            let cases = &case(&reference.data, &format!("{name}_{source}"))["results"][0]["rows"];
            for (index, row) in cases.as_array().unwrap().iter().enumerate() {
                let physical = datum(&row[0]);
                let ty = reference
                    .value_type_by_oid(physical.type_oid())
                    .unwrap()
                    .unwrap();
                let memory = MemoryBudget::new(1 << 20);
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&memory, &original, &invoking);
                let value = Value::Datum(physical);
                let rendered = format_postgres_text_with_catalog_and_control(
                    &value,
                    &ty,
                    Some(&reference),
                    &control,
                )
                .unwrap();
                assert_eq!(
                    *rendered,
                    case(&reference.data, &format!("{name}_{expected}"))["results"][0]["rows"][0]
                        [index]
                        .as_str()
                        .unwrap(),
                    "{name} {source} {index}"
                );
                assert_eq!(memory.used(), rendered.reserved_bytes());
                drop(rendered);
                assert_eq!(memory.used(), 0);
                for size in [0, 1, 8, 32, 64, 128, 256] {
                    let memory = MemoryBudget::new(size);
                    let result = format_postgres_text_with_catalog_and_control(
                        &value,
                        &ty,
                        Some(&reference),
                        &ProductionControl::new(&memory, &original, &invoking),
                    );
                    if let Err(error) = &result {
                        assert_eq!(error.sqlstate(), Some("53200"));
                    }
                    drop(result);
                    assert_eq!(memory.used(), 0, "{name} {source} {index} budget {size}");
                }
            }
        }
    }
}

#[test]
fn typed_output_admits_nested_enum_labels_and_preserves_container_text() {
    let reference = reference();
    reference.observing.set(true);
    let label = &reference.labels.labels[0];
    let value = Value::Enum(
        EnumValue::new(reference.labels.type_oid, label.key.clone())
            .with_label_oid(Some(label.oid)),
    );
    let sources = [
        (Value::Row(vec![value.clone()].into()), "(low)"),
        (Value::List(vec![value.clone()]), "[\"low\"]"),
        (
            Value::Map(std::collections::BTreeMap::from([(
                "label".into(),
                value.clone(),
            )])),
            "{\"label\":\"low\"}",
        ),
        (Value::Record(vec![("label".into(), value)].into()), "(low)"),
    ];
    for (source, expected) in sources {
        for size in [0, 32, 64, 128, 256, 4096] {
            let memory = MemoryBudget::new(size);
            let token = CancellationToken::new();
            let result = format_postgres_text_with_catalog_and_control(
                &source,
                &ColumnType::Text,
                Some(&reference),
                &ProductionControl::new(&memory, &token, &token),
            );
            match &result {
                Ok(value) => {
                    assert_eq!(**value, expected);
                    assert_eq!(memory.used(), value.reserved_bytes());
                }
                Err(error) => {
                    assert!(size < 4096);
                    assert_eq!(error.sqlstate(), Some("53200"));
                }
            }
            drop(result);
            assert_eq!(memory.used(), 0);
        }
    }
}
