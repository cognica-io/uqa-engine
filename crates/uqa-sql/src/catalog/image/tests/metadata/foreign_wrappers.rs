//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{
        ArraySliceBounds, AssignmentBinding, AssignmentPathStep, ColumnType, CompositeFieldBinding,
        FunctionBinding, FunctionValueBinding, RoutineInvocationBinding, RoutineVariadicMode,
    },
    catalog::{
        collations::CollationSelection,
        foreign_wrapper::{
            native_wrappers, ForeignWrapperDefinition, ForeignWrapperFunction,
            ForeignWrapperHandler, ForeignWrapperReference,
        },
    },
};

mod validation;
mod wire;

fn callback() -> ForeignWrapperFunction {
    ForeignWrapperFunction {
        oid: 65537,
        binding: FunctionBinding {
            object_id: Some([14; 16]),
            name: "old.f".into(),
            argument_types: vec!["int4".into()],
            builtin: false,
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: Some(CollationSelection::INDETERMINATE),
        },
    }
}
fn wrapper() -> ForeignWrapperDefinition {
    ForeignWrapperDefinition {
        name: "w".into(),
        identity: ForeignWrapperReference {
            oid: 65536,
            object_id: [13; 16],
        },
        owner: RoleIdentity::BOOTSTRAP,
        handler: ForeignWrapperHandler::None,
        validator: None,
        options: vec![("z".into(), "α".into()), ("a".into(), String::new())],
    }
}
fn function_wrapper() -> ForeignWrapperDefinition {
    ForeignWrapperDefinition {
        handler: ForeignWrapperHandler::Function(callback()),
        ..wrapper()
    }
}
fn invocation(mode: RoutineVariadicMode) -> RoutineInvocationBinding {
    RoutineInvocationBinding {
        argument_positions: vec![2, 0, 1],
        argument_targets: vec!["text[]".into(), "numeric(13,-2)".into()],
        argument_sources: vec![None, Some(String::new()), Some("old.type[]".into())],
        parameter_types: vec!["text".into(), "oid".into()],
        return_type: Some(String::new()),
        variadic_mode: mode,
    }
}
fn values() -> [Option<Box<FunctionValueBinding>>; 4] {
    let field = CompositeFieldBinding {
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
    };
    [
        None,
        Some(Box::new(FunctionValueBinding::CompositeField(
            field.clone(),
        ))),
        Some(Box::new(FunctionValueBinding::CompositeField(
            CompositeFieldBinding {
                changed_type: None,
                ..field
            },
        ))),
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
    ]
}

#[test]
fn wrapper_images_preserve_complete_invocation_value_bindings_and_legacy_absence() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    for mode in [
        RoutineVariadicMode::None,
        RoutineVariadicMode::Expanded { parameter_index: 2 },
        RoutineVariadicMode::Explicit { parameter_index: 1 },
    ] {
        for value_binding in values() {
            for contract in [None, Some(Box::new(invocation(mode)))] {
                let mut function = callback();
                function.binding.invocation = contract;
                function.binding.value_binding = value_binding.clone();
                let value = ForeignWrapperDefinition {
                    handler: ForeignWrapperHandler::Function(function.clone()),
                    validator: Some(function),
                    ..wrapper()
                };
                let source = singleton("w", value);
                let bytes = encode(&source);
                assert_round_trip(&source, &bytes);
                let decoded =
                    ForeignWrapperDefinition::decode(&mut bytes.as_slice(), &memory, &token)
                        .unwrap();
                assert_eq!(
                    serde_json::to_value(&decoded).unwrap(),
                    serde_json::to_value(&source).unwrap()
                );
                drop(decoded);
                assert_eq!(memory.used(), 0);
            }
        }
    }
    for collation in [None, Some(CollationSelection::from_oid(950).unwrap())] {
        let mut function = callback();
        function.binding.builtin = true;
        function.binding.object_id = None;
        function.binding.input_collation = collation;
        function.binding.invocation = Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![],
            argument_targets: vec![],
            argument_sources: vec![],
            parameter_types: vec![],
            return_type: None,
            variadic_mode: RoutineVariadicMode::None,
        }));
        let source = singleton(
            "w",
            ForeignWrapperDefinition {
                validator: Some(function),
                ..wrapper()
            },
        );
        let bytes = encode(&source);
        assert_round_trip(&source, &bytes);
    }
}

#[test]
fn wrapper_import_retains_exact_callback_option_and_box_admission_after_source_retirement() {
    for value_binding in values() {
        let mut function = callback();
        function.binding.invocation = Some(Box::new(invocation(RoutineVariadicMode::Explicit {
            parameter_index: 1,
        })));
        function.binding.value_binding = value_binding;
        let source = singleton(
            "w",
            ForeignWrapperDefinition {
                validator: Some(function),
                ..function_wrapper()
            },
        );
        super::retention::retained(&source);
    }
    super::retention::retained(&native_wrappers());
}

#[test]
fn wrapper_readers_bound_large_options_and_empty_contract_sequences_without_allocations() {
    let mut value = function_wrapper();
    value.options[0].1 = "α native option".repeat(32768);
    let ForeignWrapperHandler::Function(function) = &mut value.handler else {
        unreachable!()
    };
    function.binding.argument_types = vec![String::new(); 16384];
    function.binding.invocation = Some(Box::new(RoutineInvocationBinding {
        argument_positions: vec![0; 4096],
        argument_targets: vec![String::new(); 4096],
        argument_sources: vec![None; 4096],
        parameter_types: vec![String::new(); 4096],
        return_type: None,
        variadic_mode: RoutineVariadicMode::None,
    }));
    function.binding.value_binding = Some(Box::new(FunctionValueBinding::Assignment(
        AssignmentBinding {
            path: vec![
                AssignmentPathStep::Field {
                    type_oid: 0,
                    number: 0
                };
                4096
            ],
            value_argument: 0,
        },
    )));
    super::retention::bounded(&singleton("w", value));
}

#[test]
fn cancelled_or_unwound_wrapper_import_retires_partial_callback_and_option_allocations() {
    let mut value = wrapper();
    let mut function = callback();
    function.binding.invocation = Some(Box::new(invocation(RoutineVariadicMode::None)));
    function.binding.value_binding = values()[1].clone();
    value.validator = Some(function);
    let source = singleton("w", value);
    super::retention::cancellation(&source);
    struct Panics<'a> {
        data: &'a [u8],
        remaining: usize,
    }
    impl Read for Panics<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            assert!(self.remaining != 0, "injected input unwind");
            let n = out.len().min(self.remaining).min(self.data.len());
            out[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            self.remaining -= n;
            Ok(n)
        }
    }
    let bytes = encode(&source);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    for remaining in [120, bytes.len() - 10] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ForeignWrapperDefinition::decode(
                &mut Panics {
                    data: &bytes,
                    remaining,
                },
                &memory,
                &token,
            )
        }));
        assert!(result.is_err());
        assert_eq!(memory.used(), 0);
    }
}
