//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::composites::{CompositeAttribute, CompositeTypeDescriptor};
use crate::expr::{enums::EnumTypeLabels, EngineHook, SQLValueCatalog};
use crate::ColumnType;
use std::sync::Arc;

// PostgreSQL 18.4 array payloads captured through the retained bytea field in composite_catalog_array_oracle. These OIDs belong to that capture's catalog.
const ENUM: &str = "0100000020000000a852000003000000010000000500000000000000ad520000ac520000";
const DOMAIN: &str = "0100000020000000af520000030000000100000005000000000000000100000007000000";
const RECORD: &str = "0100000020000000b35200000300000001000000050000000000000080000000ffffffffb3520000ffffffff000002000200180001000000096f6e6580000000ffffffffb3520000ffffffff0000020002001800020000000974776f";

fn physical(hex: &str) -> DatumValue {
    let payload = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let mut bytes = ((payload.len() as u32 + 4) << 2).to_le_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    DatumValue::new(1009, 0, bytes)
}

struct Catalog;

impl SQLValueCatalog for Catalog {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        Ok(match oid {
            21_160 => Some(ColumnType::Enum(crate::ast::EnumTypeReference {
                schema: "public".into(),
                name: "decode_enum".into(),
                oid,
                array_oid: 21_159,
            })),
            21_167 => Some(ColumnType::Domain {
                schema: "public".into(),
                name: "decode_domain".into(),
                oid,
                array_oid: Some(21_166),
                base: Box::new(ColumnType::Integer),
            }),
            21_171 => Some(ColumnType::Composite(crate::ast::CompositeTypeReference {
                schema: "public".into(),
                name: "decode_record".into(),
                oid,
                array_oid: 21_170,
                relation_oid: 21_169,
            })),
            _ => None,
        })
    }

    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == 21_171).then(|| {
            Arc::new(CompositeTypeDescriptor {
                type_oid: oid,
                relation_oid: 21_169,
                dropped: Vec::new(),
                attributes: vec![
                    CompositeAttribute {
                        name: "n".into(),
                        number: 1,
                        ty: ColumnType::Integer,
                    },
                    CompositeAttribute {
                        name: "t".into(),
                        number: 2,
                        ty: ColumnType::Text,
                    },
                ],
            })
        }))
    }

    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        panic!("array deformation must not observe enum labels")
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("admitted values must not repeat input checks")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("array deformation must not look up enum output names")
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn catalog_array_layouts_keep_identity_nulls_and_deferred_fields() {
    let memory = MemoryBudget::new(16_384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (hex, oid) in [(ENUM, 21_160), (DOMAIN, 21_167), (RECORD, 21_171)] {
        let datum = physical(hex);
        let output = read_with_value_catalog_and_control(&datum, Some(&Catalog), &control).unwrap();
        let Value::Array(array) = &*output else {
            panic!("array")
        };
        assert_eq!(array.element_type_oid(), Some(oid));
        assert_eq!(array.dimensions(), &[3]);
        assert_eq!(array.lower_bounds(), &[1]);
        assert_eq!(array.elements()[1], Value::Null);
        match oid {
            21_160 => {
                for (index, expected) in [(0, 21_165), (2, 21_164)] {
                    let Value::Datum(label) = &array.elements()[index] else {
                        panic!("raw enum")
                    };
                    assert_eq!(label.type_oid(), oid);
                    assert_eq!(enum_label_oid(label).unwrap(), expected);
                }
            }
            21_167 => assert_eq!(
                array.elements(),
                &[Value::Int(1), Value::Null, Value::Int(7)]
            ),
            _ => {
                let Value::Record(fields) = &array.elements()[0] else {
                    panic!("record")
                };
                for ((name, value), (expected_name, expected)) in fields
                    .iter()
                    .zip([("n", Value::Int(1)), ("t", Value::Str("one".into()))])
                {
                    assert_eq!(name, expected_name);
                    let Value::Datum(field) = value else {
                        panic!("deferred field")
                    };
                    assert_eq!(read(field).unwrap(), expected);
                }
            }
        }
        assert_eq!(memory.used(), output.reserved_bytes());
        assert!(memory.used() > 0);
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn physical_array_properties_do_not_resolve_or_materialize_elements() {
    let datum = physical(ENUM);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (name, dimension, expected) in [
        ("cardinality", None, 3),
        ("array_ndims", None, 1),
        ("array_length", Some(1), 3),
        ("array_lower", Some(1), 1),
        ("array_upper", Some(1), 3),
    ] {
        let mut args = vec![Value::Datum(datum.clone())];
        args.extend(dimension.map(Value::Int));
        let output =
            crate::expr::scalar_array::eval_array_functions_with_control(name, &args, &control)
                .unwrap()
                .unwrap();
        assert_eq!(*output, Value::Int(expected));
        assert_eq!(memory.used(), 0);
    }
    let invalid = Value::Datum(DatumValue::new(1009, 0, Vec::new()));
    assert_eq!(
        *crate::expr::scalar_array::eval_array_functions_with_control(
            "array_length",
            &[invalid, Value::Null],
            &control,
        )
        .unwrap()
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        read(&datum).unwrap_err().to_string(),
        "cache lookup failed for type 21160"
    );
}

#[test]
fn catalog_array_failures_release_memory_and_honor_cancellation() {
    let token = CancellationToken::new();
    for hex in [ENUM, DOMAIN, RECORD] {
        let datum = physical(hex);
        let memory = MemoryBudget::new(64);
        let control = ProductionControl::new(&memory, &token, &token);
        assert!(read_with_value_catalog_and_control(&datum, Some(&Catalog), &control).is_err());
        assert_eq!(memory.used(), 0);
        for length in 0..datum.bytes().len() {
            let truncated = DatumValue::new(1009, 0, datum.bytes()[..length].to_vec());
            assert!(
                read_with_value_catalog_and_control(&truncated, Some(&Catalog), &control).is_err()
            );
            assert_eq!(memory.used(), 0);
        }
    }
    token.cancel();
    let memory = MemoryBudget::new(16_384);
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        read_with_value_catalog_and_control(&physical(RECORD), Some(&Catalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}

struct DomainHook;

impl EngineHook for DomainHook {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        Catalog
            .value_type_by_oid(oid)
            .map_err(|error| error.to_string())
    }
}

#[test]
fn physical_domain_arrays_use_type_metadata_without_enum_capability() {
    let value = read_with_catalog(&physical(DOMAIN), Some(&DomainHook)).unwrap();
    let Value::Array(array) = value else {
        panic!("array")
    };
    assert_eq!(array.element_type_oid(), Some(21_167));
    assert_eq!(
        array.elements(),
        &[Value::Int(1), Value::Null, Value::Int(7)]
    );
}
