//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{enums::EnumTypeLabels, RangeValueDescriptor, SQLValueCatalog};
use std::sync::Arc;

struct Metadata(Arc<RangeValueDescriptor>);

impl SQLValueCatalog for Metadata {
    fn value_range_type(&self, oid: u32) -> Result<Option<Arc<RangeValueDescriptor>>, SQLError> {
        Ok((oid == self.0.range_oid || oid == self.0.multirange_oid).then(|| Arc::clone(&self.0)))
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        panic!("no enum endpoint")
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("no input check")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("no enum endpoint")
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

#[test]
fn declared_range_fixed_and_variable_bounds_match_postgresql_physical_alignment() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../encoding-postgresql.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let endpoint = match case["endpoint"].as_str().unwrap() {
            "text" => ColumnType::Text,
            "uuid" => ColumnType::Uuid,
            "timetz" => ColumnType::TimeTz,
            "jsonb" => ColumnType::JsonB,
            "smallint" => ColumnType::SmallInteger,
            _ => unreachable!(),
        };
        let descriptor = RangeValueDescriptor {
            range_oid: case["range_oid"].as_u64().unwrap() as u32,
            multirange_oid: case["multirange_oid"].as_u64().unwrap() as u32,
            endpoint,
        };
        let members = case["bounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|bounds| {
                let bound = |index| {
                    crate::expr::cast_value(
                        &Value::Str(bounds[index].as_str().unwrap().into()),
                        &descriptor.endpoint.sql_name(),
                    )
                    .unwrap()
                };
                CanonicalRange {
                    subtype: descriptor.range_oid,
                    lower: Some(bound(0)),
                    upper: Some(bound(1)),
                    lower_inclusive: true,
                    upper_inclusive: true,
                    empty: false,
                }
            })
            .collect::<Vec<_>>();
        let ranges = CanonicalMultirange {
            subtype: descriptor.range_oid,
            ranges: members,
        };
        let node_tree::Field::Node(node) =
            node_tree::parse(case["node"].as_str().unwrap()).unwrap()
        else {
            panic!("constant")
        };
        let node_tree::Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
            panic!("physical bytes")
        };
        let catalog = Metadata(Arc::new(descriptor.clone()));
        let rendered = read_class(
            &bytes[4..],
            case["multirange"].as_bool().unwrap(),
            Some(&catalog),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        assert_eq!(
            *rendered,
            Value::Str(case["output"].as_str().unwrap().into()),
            "{} read",
            case["id"]
        );
        for limit in [0, 32, 128, 512, 2048, 32768] {
            let memory = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let output = if case["multirange"].as_bool().unwrap() {
                declared_multirange_datum_with_control(&ranges, &descriptor, None, &control)
            } else {
                declared_range_datum_with_control(&ranges.ranges[0], &descriptor, None, &control)
            };
            match &output {
                Ok(output) => {
                    let Value::Datum(datum) = &**output else {
                        panic!("physical carrier")
                    };
                    assert_eq!(datum.bytes(), bytes, "{}", case["id"]);
                    assert_eq!(output.reserved_bytes(), datum.retained_bytes());
                    assert_eq!(memory.used(), output.reserved_bytes());
                }
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"), "{}: {error}", case["id"]);
                }
            }
            if limit == 32768 {
                assert!(output.is_ok());
            }
            drop(output);
            assert_eq!(memory.used(), 0);
        }
    }
}
