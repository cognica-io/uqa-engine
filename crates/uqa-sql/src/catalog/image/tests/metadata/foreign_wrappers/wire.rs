//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn prefix() -> Vec<u8> {
    let mut bytes =
        b"UQAFWR01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0w\x01\0\0\0\0\0\0\0w\0\0\x01\0".to_vec();
    bytes.extend_from_slice(&[13; 16]);
    bytes.extend_from_slice(b"\x0a\0\0\0\0\0\0\0UQA:role00000010\x02\0\0\0\0\0\0\0");
    bytes
}
fn options(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(
        b"\x01\0\0\0\0\0\0\0z\x02\0\0\0\0\0\0\0\xce\xb1\x01\0\0\0\0\0\0\0a\0\0\0\0\0\0\0\0",
    );
}

#[test]
fn wrapper_images_match_literal_none_and_native_handler_tags_with_ordered_options() {
    let mut bytes = prefix();
    bytes.extend_from_slice(&[0, 0]);
    options(&mut bytes);
    assert_round_trip(&singleton("w", wrapper()), &bytes);
    for (tag, native) in [
        (0, NativeForeignWrapper::Memory),
        (1, NativeForeignWrapper::DuckDB),
        (2, NativeForeignWrapper::Arrow),
    ] {
        let mut bytes = prefix();
        bytes.extend_from_slice(&[1, tag, 0]);
        options(&mut bytes);
        assert_round_trip(
            &singleton(
                "w",
                ForeignWrapperDefinition {
                    handler: ForeignWrapperHandler::Native(native),
                    ..wrapper()
                },
            ),
            &bytes,
        );
    }
    let natives = native_wrappers();
    let bytes = encode(&natives);
    assert_round_trip(&natives, &bytes);
    assert_round_trip(
        &NamedDefinitions::<ForeignWrapperDefinition>::new(),
        b"UQAFWR01\0\0\0\0\0\0\0\0",
    );
}

#[test]
fn wrapper_callback_image_matches_literal_original_function_identity_and_contract_absence() {
    let mut bytes = prefix();
    bytes.extend_from_slice(b"\x02\x01\0\x01\0\x01");
    bytes.extend_from_slice(&[14; 16]);
    bytes.extend_from_slice(
        b"\x05\0\0\0\0\0\0\0old.f\0\x01\0\0\0\0\x01\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0int4\0\0\0",
    );
    options(&mut bytes);
    assert_round_trip(&singleton("w", function_wrapper()), &bytes);
}

#[test]
fn wrapper_callback_contract_and_assignment_match_literal_fields_and_slice_masks() {
    let mut function = callback();
    function.binding.invocation = Some(Box::new(RoutineInvocationBinding {
        argument_positions: vec![1],
        argument_targets: vec!["int4".into()],
        argument_sources: vec![None],
        parameter_types: vec!["text".into()],
        return_type: Some("bool".into()),
        variadic_mode: RoutineVariadicMode::Expanded { parameter_index: 0 },
    }));
    function.binding.value_binding = Some(Box::new(FunctionValueBinding::Assignment(
        AssignmentBinding {
            path: vec![
                AssignmentPathStep::Field {
                    type_oid: 65538,
                    number: 2,
                },
                AssignmentPathStep::Subscripts {
                    first_argument: 1,
                    dimensions: 2,
                    slice: true,
                    bounds: ArraySliceBounds::from_stored_masks([0x81, 0xc2]),
                },
            ],
            value_argument: 3,
        },
    )));
    let mut bytes = prefix();
    bytes.extend_from_slice(b"\x02\x01\0\x01\0\x01");
    bytes.extend_from_slice(&[14; 16]);
    bytes.extend_from_slice(
        b"\x05\0\0\0\0\0\0\0old.f\0\x01\0\0\0\0\x01\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0int4",
    );
    bytes.extend_from_slice(b"\x01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\x04\0\0\0\0\0\0\0bool");
    let variadic = bytes.len();
    bytes.extend_from_slice(
        b"\x01\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0int4\0\x04\0\0\0\0\0\0\0text",
    );
    bytes.extend_from_slice(b"\x02\x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0");
    let path_tag = bytes.len();
    bytes.extend_from_slice(
        b"\0\x02\0\x01\0\x02\0\x01\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x01\x81\xc2\0",
    );
    options(&mut bytes);
    let source = singleton(
        "w",
        ForeignWrapperDefinition {
            handler: ForeignWrapperHandler::Function(function),
            ..wrapper()
        },
    );
    assert_round_trip(&source, &bytes);
    super::super::validation::malformed(&source);
    for (position, tag) in [(variadic, 3), (path_tag, 2), (path_tag + 24, 2)] {
        let mut invalid = bytes.clone();
        invalid[position] = tag;
        super::super::validation::rejects::<ForeignWrapperDefinition>(&invalid);
    }
}
