//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    ArraySliceBounds, AssignmentBinding, AssignmentPathStep, ColumnType, CompositeFieldBinding,
    FunctionBinding, FunctionDispatch, FunctionResolutionError, FunctionValueBinding,
    OperatorResolutionError, RoutineInvocationBinding, RoutineVariadicMode,
};

mod dispatch;
mod validation;

fn binding() -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: "f".into(),
        argument_types: vec![],
        builtin: true,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }
}

fn encode(value: &FunctionBinding) -> Vec<u8> {
    streamed(value.image_reader(&CancellationToken::new()).unwrap(), 13)
}

fn assert_restored(value: &FunctionBinding, bytes: &[u8]) {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let restored = FunctionBinding::read_image(&mut &*bytes, &memory, &token).unwrap();
    assert_eq!(&*restored, value);
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

fn retained_binding() -> FunctionBinding {
    FunctionBinding {
        object_id: Some([17; 16]),
        name: "\"original.schema\".\"f\"".into(),
        argument_types: vec!["numeric(13,-2)".into(), "old.type[]".into()],
        builtin: false,
        dispatch: Some(FunctionDispatch::FieldSelect),
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![2, 0, 1],
            argument_targets: vec!["text[]".into(), "old.type".into()],
            argument_sources: vec![None, Some(String::new()), Some("old.type".into())],
            parameter_types: vec!["text".into(), "oid".into()],
            return_type: Some(String::new()),
            variadic_mode: RoutineVariadicMode::Explicit { parameter_index: 1 },
        })),
        value_binding: Some(Box::new(FunctionValueBinding::CompositeField(
            CompositeFieldBinding {
                type_oid: 65538,
                number: 2,
                result_type: ColumnType::Array(Box::new(ColumnType::Varchar(Some(13)))),
                collation_oid: Some(0),
                dropped: true,
                changed_type: Some(ColumnType::Domain {
                    schema: "original".into(),
                    name: "d".into(),
                    oid: 65539,
                    array_oid: None,
                    collation_oid: Some(950),
                    base: Box::new(ColumnType::Text),
                }),
            },
        ))),
        resolution_error: Some(Box::new(FunctionResolutionError::Operator(Box::new(
            OperatorResolutionError {
                sqlstate: "42883".into(),
                message: "operator does not exist: old.type + text".into(),
            },
        )))),
        input_collation: Some(crate::catalog::collations::CollationSelection::INDETERMINATE),
    }
}

#[test]
fn function_binding_image_matches_independent_wire_fixture() {
    let value = FunctionBinding {
        dispatch: Some(FunctionDispatch::ValueNullTest { negated: true }),
        ..binding()
    };
    // Native version/count, absent identity, name, builtin, absent collation, argument count, absent invocation/value, selected dispatch and absent error.
    let expected =
        b"UQAFNB01\x01\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0f\x01\0\0\0\0\0\0\0\0\0\0\0\x01\x16\x01\0";
    assert_eq!(encode(&value), expected);
    assert_restored(&value, expected);
}

#[test]
fn deferred_function_failures_match_independent_wire_tags_and_original_text() {
    let base = encode(&binding());
    for (error, tail) in [
        (
            FunctionResolutionError::UndefinedFunction {
                signature: "f(text)".into(),
            },
            b"\x01\x07\0\0\0\0\0\0\0f(text)".as_slice(),
        ),
        (
            FunctionResolutionError::Operator(Box::new(OperatorResolutionError {
                sqlstate: "42883".into(),
                message: "α".into(),
            })),
            b"\x02\x05\0\0\0\0\0\0\x0042883\x02\0\0\0\0\0\0\0\xce\xb1".as_slice(),
        ),
    ] {
        let value = FunctionBinding {
            resolution_error: Some(Box::new(error)),
            ..binding()
        };
        let mut expected = base[..base.len() - 1].to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(encode(&value), expected);
        assert_restored(&value, &expected);
    }
}

#[test]
fn native_binding_images_retain_invocation_value_and_deferred_failure_metadata() {
    let original = retained_binding();
    for mode in [
        RoutineVariadicMode::None,
        RoutineVariadicMode::Expanded { parameter_index: 0 },
        RoutineVariadicMode::Explicit { parameter_index: 1 },
    ] {
        for error in [
            None,
            original.resolution_error.clone(),
            Some(Box::new(FunctionResolutionError::UndefinedFunction {
                signature: "missing(\"old.type\", text)".into(),
            })),
        ] {
            for value_binding in [
                None,
                original.value_binding.clone(),
                Some(Box::new(FunctionValueBinding::Assignment(
                    AssignmentBinding {
                        path: vec![
                            AssignmentPathStep::Field {
                                type_oid: 65538,
                                number: 2,
                            },
                            AssignmentPathStep::Subscripts {
                                first_argument: 3,
                                dimensions: 6,
                                slice: true,
                                bounds: ArraySliceBounds::from_stored_masks([0x81, 0xc2]),
                            },
                        ],
                        value_argument: 15,
                    },
                ))),
            ] {
                let mut value = original.clone();
                value.invocation.as_mut().unwrap().variadic_mode = mode;
                value.resolution_error.clone_from(&error);
                value.value_binding = value_binding;
                let bytes = encode(&value);
                assert_restored(&value, &bytes);
                for width in [1, 7, 4096, 20000] {
                    assert_eq!(
                        streamed(
                            value.image_reader(&CancellationToken::new()).unwrap(),
                            width
                        ),
                        bytes
                    );
                }
            }
        }
    }
}

#[test]
fn imported_native_binding_retains_exact_payload_and_box_admission() {
    let value = retained_binding();
    let bytes = encode(&value);
    let token = CancellationToken::new();
    let ample = MemoryBudget::new(16 << 20);
    let restored = FunctionBinding::read_image(&mut bytes.as_slice(), &ample, &token).unwrap();
    let peak = ample.peak();
    let retained = ample.used();
    assert!(retained > 0);
    drop(restored);
    assert_eq!(ample.used(), 0);
    for limit in [0, 1, retained - 1, peak - 1] {
        let memory = MemoryBudget::new(limit);
        let error = FunctionBinding::read_image(&mut bytes.as_slice(), &memory, &token)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    let restored = FunctionBinding::read_image(&mut bytes.as_slice(), &exact, &token).unwrap();
    drop(value);
    drop(bytes);
    assert_eq!(exact.used(), retained);
    assert_eq!(restored.name, "\"original.schema\".\"f\"");
    assert_eq!(
        restored
            .resolution_error
            .as_ref()
            .unwrap()
            .sql_error()
            .sqlstate(),
        Some("42883")
    );
    drop(restored);
    assert_eq!(exact.used(), 0);
}

#[test]
fn native_binding_output_bounds_large_fields_without_allocation() {
    let mut value = retained_binding();
    value.name = "original-α".repeat(32768);
    value.argument_types = vec![String::new(); 16384];
    value.resolution_error = Some(Box::new(FunctionResolutionError::UndefinedFunction {
        signature: "large-α".repeat(32768),
    }));
    let token = CancellationToken::new();
    let expected = encode(&value);
    let mut buffer = vec![0; 20000];
    let mut offset = 0;
    let allocations = allocation_counter::measure(|| {
        let mut reader = value.image_reader(&token).unwrap();
        loop {
            let n = reader.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            assert!(n <= 16384);
            assert_eq!(&buffer[..n], &expected[offset..offset + n]);
            offset += n;
        }
    });
    assert_eq!(offset, expected.len());
    assert_eq!(allocations.count_total, 0);
    assert_restored(&value, &expected);
}
