//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CompositeTypeReference, EnumTypeReference},
    catalog::node_tree::{self, Field},
    expr::{
        composites::{CompositeAttribute, CompositeTypeDescriptor},
        enums::{EnumTypeLabel, EnumTypeLabels},
    },
};
use std::{cell::Cell, sync::Arc};
use uqa_core::{memory::MemoryBudget, CancellationToken, EnumLabelKey, EnumValue};

mod ranges;

struct Reference {
    data: serde_json::Value,
    labels: Arc<EnumTypeLabels>,
    record: Arc<CompositeTypeDescriptor>,
    observing: Cell<bool>,
}

impl Reference {
    fn new() -> Self {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("postgresql.json")).unwrap();
        Self::from_data(data)
    }

    fn from_data(data: serde_json::Value) -> Self {
        let rows = |id| case(&data, id)["results"][0]["rows"].as_array().unwrap();
        let labels = rows("enum_labels");
        let labels = Arc::new(EnumTypeLabels {
            type_oid: integer(&labels[0][0]),
            labels: labels
                .iter()
                .enumerate()
                .map(|(index, row)| EnumTypeLabel {
                    oid: integer(&row[1]),
                    key: EnumLabelKey::from_bytes(vec![(index + 1) as u8]).unwrap(),
                    label: row[2].as_str().unwrap().into(),
                })
                .collect(),
        });
        let record_oid = rows("type_metadata")
            .iter()
            .find(|row| row[0] == "record_window")
            .unwrap();
        let record = Arc::new(CompositeTypeDescriptor {
            type_oid: integer(&record_oid[3]),
            relation_oid: 0,
            dropped: Vec::new(),
            attributes: vec![
                CompositeAttribute {
                    name: "n".into(),
                    number: 1,
                    ty: ColumnType::Integer,
                    collation_oid: None,
                },
                CompositeAttribute {
                    name: "s".into(),
                    number: 2,
                    ty: ColumnType::Text,
                    collation_oid: None,
                },
            ],
        });
        Self {
            data,
            labels,
            record,
            observing: Cell::new(false),
        }
    }

    fn endpoint(&self, name: &str) -> ColumnType {
        let row = self.data["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == "type_metadata")
            .unwrap()["results"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[0] == name)
            .unwrap();
        let oid = integer(&row[3]);
        match name {
            "enum_window" => ColumnType::Enum(EnumTypeReference {
                schema: "public".into(),
                name: "range_mood".into(),
                oid,
                array_oid: 0,
            }),
            "domain_window" => ColumnType::Domain {
                collation_oid: None,
                schema: "public".into(),
                name: "range_positive".into(),
                oid,
                array_oid: None,
                base: Box::new(ColumnType::Integer),
            },
            "text_window" => ColumnType::Text,
            "array_window" => ColumnType::Array(Box::new(ColumnType::Integer)),
            "record_window" => ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: "range_pair".into(),
                oid,
                array_oid: 0,
                relation_oid: 0,
            }),
            _ => unreachable!(),
        }
    }

    fn metadata(&self, name: &str) -> &serde_json::Value {
        case(&self.data, "type_metadata")["results"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[0] == name)
            .unwrap()
    }

    fn range_type(&self, name: &str) -> ColumnType {
        let row = self.metadata(name);
        ColumnType::DeclaredRange(crate::ast::RangeTypeReference {
            schema: "public".into(),
            name: name.into(),
            oid: integer(&row[1]),
            array_oid: integer(&row[2]),
            alignment: crate::ast::RangeAlignment::for_endpoint(&self.endpoint(name)),
        })
    }

    fn bytes(&self, name: &str) -> Vec<u8> {
        let node = &case(&self.data, &format!("{name}_catalog"))["results"][0]["rows"][0][0];
        let Field::Node(node) = node_tree::parse(node.as_str().unwrap()).unwrap() else {
            panic!("constant")
        };
        let Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
            panic!("physical constant")
        };
        bytes.clone()
    }
}

fn case<'a>(data: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()
}

fn integer(value: &serde_json::Value) -> u32 {
    value.as_str().unwrap().parse().unwrap()
}

impl SQLValueCatalog for Reference {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        if oid == self.record.type_oid {
            return Ok(Some(self.endpoint("record_window")));
        }
        for name in FAMILIES {
            let row = self.metadata(name);
            let range = self.range_type(name);
            if oid == integer(&row[1]) {
                return Ok(Some(range));
            }
            if oid == integer(&row[2]) {
                return Ok(Some(ColumnType::Array(Box::new(range))));
            }
            if oid == integer(&row[6]) {
                let ColumnType::DeclaredRange(mut reference) = range else {
                    unreachable!()
                };
                reference.oid = oid;
                reference.name.push_str("_multirange");
                reference.array_oid = 0;
                return Ok(Some(ColumnType::DeclaredMultirange(reference)));
            }
        }
        Ok(None)
    }
    fn value_range_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<crate::expr::range::RangeValueDescriptor>>, SQLError> {
        Ok(FAMILIES.iter().find_map(|name| {
            let row = self.metadata(name);
            (oid == integer(&row[1]) || oid == integer(&row[6])).then(|| {
                Arc::new(crate::expr::range::RangeValueDescriptor {
                    range_oid: integer(&row[1]),
                    multirange_oid: integer(&row[6]),
                    endpoint: self.endpoint(name),
                })
            })
        }))
    }
    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == self.record.type_oid).then(|| Arc::clone(&self.record)))
    }
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        assert!(
            self.observing.get(),
            "deformation must retain enum identity without observing labels"
        );
        Ok((oid == self.labels.type_oid).then(|| Arc::clone(&self.labels)))
    }
    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>, SQLError> {
        assert!(self.observing.get());
        Ok(self
            .labels
            .labels
            .iter()
            .find(|label| label.oid == oid)
            .map(|label| {
                EnumValue::new(self.labels.type_oid, label.key.clone()).with_label_oid(Some(oid))
            }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("admitted endpoints must not repeat input safety checks")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("no input diagnostic")
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

const FAMILIES: [&str; 5] = [
    "enum_window",
    "domain_window",
    "text_window",
    "array_window",
    "record_window",
];

#[test]
fn physical_range_endpoints_preserve_postgresql_typed_values_without_repeating_input() {
    let mut reference = Reference::new();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for name in FAMILIES {
        let bytes = reference.bytes(name);
        let ty = reference.endpoint(name);
        let expected = &case(&reference.data, &format!("{name}_bounds"))["results"][0]["rows"][0];
        let mut offset = 8;
        for bound in 1..=2 {
            reference.observing.set(false);
            let value = read(
                &bytes[..bytes.len() - 1],
                &mut offset,
                0,
                &ty,
                Some(&reference),
                &control,
            )
            .unwrap();
            assert_eq!(value.reserved_bytes(), memory.used());
            reference.observing.set(true);
            assert_eq!(
                crate::result::format_postgres_text_with_catalog(&value, &ty, Some(&reference))
                    .unwrap(),
                expected[bound].as_str().unwrap(),
                "{name}"
            );
            drop(value);
            assert_eq!(memory.used(), 0);
        }
        assert_eq!(offset, bytes.len() - 1, "{name}");
    }
    let bytes = reference.bytes("enum_window");
    let ty = reference.endpoint("enum_window");
    reference.observing.set(false);
    let lower = read(&bytes, &mut 8, 0, &ty, Some(&reference), &control).unwrap();
    let Value::Datum(label) = &*lower else {
        panic!("retain the physical enum identity")
    };
    assert_eq!(
        super::super::enum_label_oid(label).unwrap(),
        reference.labels.labels[0].oid
    );
    Arc::make_mut(&mut reference.labels).labels[0].label = "lower".into();
    reference.observing.set(true);
    assert_eq!(
        crate::result::format_postgres_text_with_catalog(&lower, &ty, Some(&reference)).unwrap(),
        case(&reference.data, "enum_after_rename")["results"][0]["rows"][0][1]
            .as_str()
            .unwrap()
    );
    drop(lower);
    assert_eq!(memory.used(), 0);
}

#[test]
fn physical_endpoint_failures_release_memory_and_observe_both_cancellation_owners() {
    let reference = Reference::new();
    for name in FAMILIES {
        let bytes = reference.bytes(name);
        let ty = reference.endpoint(name);
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(1 << 20);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut end = 8;
        drop(read(&bytes, &mut end, 0, &ty, Some(&reference), &control).unwrap());
        for length in 8..end {
            assert_eq!(
                read(&bytes[..length], &mut 8, 0, &ty, Some(&reference), &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("XX001"),
                "{name}, length {length}"
            );
            assert_eq!(memory.used(), 0);
        }
        let empty = MemoryBudget::new(0);
        let limited = ProductionControl::new(&empty, &token, &token);
        let result = read(&bytes, &mut 8, 0, &ty, Some(&reference), &limited);
        if name == "domain_window" {
            assert_eq!(*result.unwrap(), Value::Int(1));
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        }
        assert_eq!(empty.used(), 0);
        for invoking_cancelled in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            if invoking_cancelled {
                invoking.cancel();
            } else {
                original.cancel();
            }
            assert_eq!(
                read(&bytes, &mut 8, 0, &ty, Some(&reference), &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
