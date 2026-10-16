//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Producers are checked against independently captured `PostgreSQL` constants, not their own decoder alone.

use super::*;
use crate::expr::{
    declared_multirange_datum_with_control, declared_range_datum_with_control,
    range_from_bounds_with_operations, RangeComparisonValue, RangeConstruction,
};
use uqa_core::{memory::Produced, ArrayValue};

impl crate::expr::composites::CompositeTypeCatalog for Reference {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        self.value_composite_type(oid)
    }
}

struct Construct<'a> {
    reference: &'a Reference,
    oid: u32,
}

impl RangeConstruction for Construct<'_> {
    type Identity = u32;
    fn identity(&self) -> u32 {
        self.oid
    }
    fn has_canonical_function(&self) -> bool {
        false
    }
    fn input_bound(
        &self,
        _: &Value,
        _: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        panic!("typed production must not repeat input")
    }
    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        self.reference
            .compare_range_bounds(self.oid, left, right, control)
    }
}

fn bound(value: &Value, control: &ProductionControl<'_>) -> Produced<Option<Value>> {
    let (value, memory) = control.copy_value(value).unwrap().into_parts();
    control.finish(Some(value), memory).unwrap()
}

fn fresh_bounds(reference: &Reference, name: &str) -> [Value; 2] {
    match name {
        "enum_window" => [0, reference.labels.labels.len() - 1].map(|index| {
            let label = &reference.labels.labels[index];
            Value::Enum(
                EnumValue::new(reference.labels.type_oid, label.key.clone())
                    .with_label_oid(Some(label.oid)),
            )
        }),
        "domain_window" => [Value::Int(1), Value::Int(3)],
        "text_window" => [Value::Str("alpha".into()), Value::Str("zulu".into())],
        "array_window" => [[1, 2], [3, 4]].map(|values| {
            Value::Array(ArrayValue::try_new(values.into_iter().map(Value::Int).collect()).unwrap())
        }),
        "record_window" => [(1, "alpha"), (3, "zulu")].map(|(number, text)| {
            Value::Record(
                vec![
                    ("n".into(), Value::Int(number)),
                    ("s".into(), Value::Str(text.into())),
                ]
                .into(),
            )
        }),
        _ => unreachable!(),
    }
}

#[test]
fn declared_range_production_matches_postgresql_bytes_from_fresh_typed_endpoints() {
    let reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        let oid = integer(&reference.metadata(name)[1]);
        let descriptor = reference.value_range_type(oid).unwrap().unwrap();
        let [lower, upper] = fresh_bounds(&reference, name);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let range = range_from_bounds_with_operations(
            bound(&lower, &control),
            bound(&upper, &control),
            true,
            true,
            &Construct {
                reference: &reference,
                oid,
            },
            &control,
        )
        .unwrap();
        let before = memory.used();
        let output =
            declared_range_datum_with_control(&range, &descriptor, Some(&reference), &control)
                .unwrap();
        let Value::Datum(physical) = &*output else {
            panic!("physical range")
        };
        assert_eq!(physical.type_oid(), oid);
        assert_eq!(physical.bytes(), reference.bytes(name), "{name}");
        let expected = node_tree::parse(
            case(&reference.data, &format!("{name}_catalog"))["results"][0]["rows"][0][0]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            Field::Node(
                node_tree::constant(&output, &reference.range_type(name), None, None).unwrap()
            ),
            expected
        );
        assert_eq!(output.reserved_bytes(), physical.retained_bytes());
        assert_eq!(memory.used(), before + output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), before);
        drop(range);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn declared_range_and_multirange_serialization_preserves_postgresql_empty_and_nested_values() {
    let reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        for source in ["catalog", "multi_catalog", "empty_catalog"] {
            for row in case(&reference.data, &format!("{name}_{source}"))["results"][0]["rows"]
                .as_array()
                .unwrap()
            {
                let input = datum(&row[0]);
                let Some(descriptor) = reference.value_range_type(input.type_oid()).unwrap() else {
                    continue;
                };
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&memory, &token, &token);
                let value = Value::Datum(input.clone());
                let selected = RangeComparisonValue::from_declared_with_control(
                    &value,
                    input.type_oid(),
                    Some(&reference),
                    &control,
                )
                .unwrap()
                .unwrap();
                let before = memory.used();
                let output = match &selected {
                    RangeComparisonValue::DeclaredRange(range) => {
                        declared_range_datum_with_control(
                            range,
                            &descriptor,
                            Some(&reference),
                            &control,
                        )
                    }
                    RangeComparisonValue::DeclaredMultirange(ranges) => {
                        declared_multirange_datum_with_control(
                            ranges,
                            &descriptor,
                            Some(&reference),
                            &control,
                        )
                    }
                    _ => unreachable!(),
                }
                .unwrap();
                let Value::Datum(physical) = &*output else {
                    panic!("physical range")
                };
                assert_eq!(physical, &input, "{name} {source}");
                let ty = reference
                    .value_type_by_oid(input.type_oid())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    Field::Node(node_tree::constant(&output, &ty, None, None).unwrap()),
                    node_tree::parse(row[0].as_str().unwrap()).unwrap()
                );
                assert_eq!(memory.used(), before + output.reserved_bytes());
                drop(output);
                drop(selected);
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn declared_range_encoding_preserves_original_quota_and_cancellation_owners() {
    let reference = reference();
    reference.observing.set(true);
    for name in FAMILIES {
        let oid = integer(&reference.metadata(name)[1]);
        let descriptor = reference.value_range_type(oid).unwrap().unwrap();
        let [lower, upper] = fresh_bounds(&reference, name);
        for limit in [0, 32, 64, 128, 256, 1024, 8192] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let memory = MemoryBudget::new(limit);
            let control = ProductionControl::new(&memory, &original, &invoking);
            // These are caller-owned inputs; encoding borrows them and admits its own output and scratch only.
            let ordinary = ProductionControl::uncontrolled();
            let range = range_from_bounds_with_operations(
                bound(&lower, &ordinary),
                bound(&upper, &ordinary),
                true,
                true,
                &Construct {
                    reference: &reference,
                    oid,
                },
                &ordinary,
            )
            .unwrap();
            let result =
                declared_range_datum_with_control(&range, &descriptor, Some(&reference), &control);
            if let Err(error) = &result {
                assert_eq!(error.sqlstate(), Some("53200"), "{name}: {error}");
            }
            if limit == 8192 {
                assert!(result.is_ok());
            }
            drop(result);
            assert_eq!(memory.used(), 0);
            for original_cancelled in [false, true] {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&memory, &original, &invoking);
                if original_cancelled {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    declared_range_datum_with_control(
                        &range,
                        &descriptor,
                        Some(&reference),
                        &control
                    )
                    .unwrap_err()
                    .sqlstate(),
                    Some("57014")
                );
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn declared_range_encoding_releases_partial_output_after_catalog_failure_or_cancellation() {
    struct Interrupted<'a> {
        descriptor: Arc<CompositeTypeDescriptor>,
        token: &'a CancellationToken,
        fail: bool,
        calls: Cell<usize>,
    }
    impl crate::expr::composites::CompositeTypeCatalog for Interrupted<'_> {
        fn composite_type(&self, _: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
            self.calls.set(self.calls.get() + 1);
            self.token.cancel();
            if self.fail {
                return Err(SQLError::Routine {
                    sqlstate: "P0001".into(),
                    message: "selected descriptor failure".into(),
                });
            }
            Ok(Some(Arc::clone(&self.descriptor)))
        }
    }
    let reference = reference();
    reference.observing.set(true);
    let oid = integer(&reference.metadata("record_window")[1]);
    let descriptor = reference.value_range_type(oid).unwrap().unwrap();
    let [lower, upper] = fresh_bounds(&reference, "record_window");
    let ordinary = ProductionControl::uncontrolled();
    let range = range_from_bounds_with_operations(
        bound(&lower, &ordinary),
        bound(&upper, &ordinary),
        true,
        true,
        &Construct {
            reference: &reference,
            oid,
        },
        &ordinary,
    )
    .unwrap();
    for fail in [false, true] {
        for cancel_original in [false, true] {
            let memory = MemoryBudget::new(8192);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let prior = control
                .copy_value(&Value::Str("prior owner".repeat(8)))
                .unwrap();
            let before = memory.used();
            let catalog = Interrupted {
                descriptor: Arc::clone(&reference.record),
                token: if cancel_original {
                    &original
                } else {
                    &invoking
                },
                fail,
                calls: Cell::new(0),
            };
            let error =
                declared_range_datum_with_control(&range, &descriptor, Some(&catalog), &control)
                    .unwrap_err();
            assert_eq!(error.sqlstate(), Some(if fail { "P0001" } else { "57014" }));
            assert_eq!(catalog.calls.get(), 1);
            assert_eq!(memory.used(), before);
            assert_eq!(*prior, Value::Str("prior owner".repeat(8)));
            drop(prior);
            assert_eq!(memory.used(), 0);
        }
    }
}
